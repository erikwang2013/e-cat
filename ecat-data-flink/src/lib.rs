// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Flink SQL Gateway client (REST API v1).
//!
//! 协议按官方文档与 Flink 源码核对（1.20.3 文档 / master 源码，2026-10-29）：
//!
//! - `POST /v1/sessions`，请求 `{"properties": {...}}` → `{"sessionHandle": ...}`
//! - `POST /v1/sessions/{h}/statements`，请求 `{"statement": sql}` →
//!   `{"operationHandle": ...}`
//! - `GET /v1/sessions/{h}/operations/{op}/status` → `{"status": ...}`
//! - `GET /v1/sessions/{h}/operations/{op}/result/{token}?rowFormat=JSON` →
//!   `{"resultType","results":{"columns","data"},"nextResultUri"}`
//!
//! 结果端点是 **`result/{token}`（单数）**，不是 `results/0`；任务书写的
//! `results/0` 与官方 OpenAPI 不符，按官方实现。`data` 的行形状是
//! `{"kind":"INSERT","fields":[...]}`（`ResultInfoSerializer`），不是裸数组。
//!
//! 错误响应透出服务器原文：凭据走 Authorization 头（不在 URL 里），不泄漏 secret。

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, map_breaker_error,
    run_with_timeout,
};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore, SemaphorePermit};

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

/// 轮询操作状态 / 结果尚未就绪时的间隔。外层 `query_timeout_secs` 覆盖整段等待。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Deserialize)]
pub struct FlinkConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// 建 session 时原样传给 gateway 的 `properties`（如 `execution.runtime-mode`）。
    #[serde(default)]
    pub session_properties: HashMap<String, String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，覆盖「建 session + 提交 + 轮询 + 拉结果」全流程；
    /// 与 reqwest 自带的总超时（`from_config` 建的 client 有 30 秒、
    /// `new` / `with_auth` 没有）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    /// 没有总开关：真要停用只能把阈值调到不可能触发（如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = **不限并发**；未配置 = 32。上限由本 crate 的信号量实现。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct FlinkClient {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    session_properties: HashMap<String, String>,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
    /// 懒创建的 session。`None` = 尚未创建或刚刚失效。
    session: Mutex<Option<String>>,
}

/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

/// 流程内部的错误：`Expired` 表示 404，需要清 session 后重放一次。
enum FlowError {
    Expired,
    Rdbms(RdbmsError),
}

impl From<RdbmsError> for FlowError {
    fn from(e: RdbmsError) -> Self {
        Self::Rdbms(e)
    }
}

impl FlowError {
    fn into_rdbms(self) -> RdbmsError {
        match self {
            Self::Rdbms(e) => e,
            Self::Expired => RdbmsError::Database("flink: session not found (HTTP 404)".into()),
        }
    }
}

impl FlinkClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            username: None,
            password: None,
            session_properties: HashMap::new(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
            session: Mutex::new(None),
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let mut client = Self::new(base_url);
        client.username = Some(username.into());
        client.password = Some(password.into());
        client
    }

    pub fn from_config(cfg: FlinkConfig) -> Result<Self, RdbmsError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| RdbmsError::Config(format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // `metrics` feature 下构造即注册（幂等：同一 backend 重复注册是覆盖闭包）。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            username: cfg.username,
            password: cfg.password,
            session_properties: cfg.session_properties,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
            semaphore: match cfg.max_concurrency {
                // `0` = 不限并发：不建信号量（`Semaphore::new(0)` 会让每次调用挂死）。
                Some(0) => None,
                Some(n) => Some(Arc::new(Semaphore::new(n))),
                None => Some(Arc::new(Semaphore::new(32))),
            },
            session: Mutex::new(None),
        })
    }

    /// 本 client 的熔断器（metrics 注册与测试用）。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 取一个并发许可；不限并发（`max_concurrency: 0`）时返回 `None`。
    /// 信号量从不 `close()`，`AcquireError` 不可达。
    async fn permit(&self) -> Option<SemaphorePermit<'_>> {
        match &self.semaphore {
            Some(sem) => Some(sem.acquire().await.expect("semaphore is never closed")),
            None => None,
        }
    }

    /// 一次调用的公共外壳：**许可 → 熔断 → 超时**。
    ///
    /// 顺序不能改：超时若在外层，熔断器会对卡死的后端永久失明 ——
    /// 超时触发时 `timeout` 会 drop 内层 future，而熔断器记失败的语句在
    /// `f().await` 之后，窗口里什么都不记。许可在最外：排队的请求还没碰后端，
    /// 不该计入熔断失败、也不该被超时掐断。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, RdbmsError>
    where
        F: std::future::Future<Output = Result<T, RdbmsError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Rdbms, self.query_timeout, fut))
            .await
            .map_err(map_breaker_error)
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        ecat_tls::apply_basic_auth(req, &self.username, &self.password)
    }

    /// 一次 JSON 请求。`404` 一律当作 session 失效 —— Flink 对未知 session/
    /// operation 的应答；调用方清缓存后重放一次。
    ///
    /// shortcut: 404 无法区分「session 过期」与「operation 丢失」，重放会让 DML
    /// 再执行一遍；网关重启丢 operation 时才会触发，需要幂等 DML 才能安全重放。
    async fn send_json(
        &self,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<serde_json::Value, FlowError> {
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("flink: {what}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| RdbmsError::Database(format!("flink: {what}: {e}")))?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(FlowError::Expired);
        }
        if !status.is_success() {
            return Err(
                RdbmsError::Database(format!("flink: {what}: HTTP {status}: {body}")).into(),
            );
        }
        serde_json::from_str(&body)
            .map_err(|e| RdbmsError::Database(format!("flink parse {what}: {e}")).into())
    }

    /// 「建 session + 提交 + 轮询 + 拉结果」全流程；404 时清缓存重放**一次**。
    async fn flow(&self, sql: &str, want_rows: bool) -> Result<Vec<Row>, RdbmsError> {
        match self.run(sql, want_rows).await {
            Err(FlowError::Expired) => {
                *self.session.lock().await = None;
                self.run(sql, want_rows)
                    .await
                    .map_err(FlowError::into_rdbms)
            }
            other => other.map_err(FlowError::into_rdbms),
        }
    }

    async fn run(&self, sql: &str, want_rows: bool) -> Result<Vec<Row>, FlowError> {
        let session = self.session().await?;
        let operation = self.submit(&session, sql).await?;
        self.wait_finished(&session, &operation).await?;
        if want_rows {
            self.fetch_results(&session, &operation).await
        } else {
            Ok(Vec::new())
        }
    }

    /// 懒创建并复用 session。锁**跨建会话的 I/O**持有：并发首调只建一个 session。
    async fn session(&self) -> Result<String, FlowError> {
        let mut cached = self.session.lock().await;
        if let Some(handle) = cached.as_ref() {
            return Ok(handle.clone());
        }
        let handle = self.open_session().await?;
        *cached = Some(handle.clone());
        Ok(handle)
    }

    async fn open_session(&self) -> Result<String, FlowError> {
        let req = self
            .client
            .post(format!("{}/v1/sessions", self.base_url))
            .json(&serde_json::json!({ "properties": self.session_properties }));
        let body = self.send_json(req, "open session").await?;
        body.get("sessionHandle")
            .and_then(|h| h.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                RdbmsError::Database("flink: open session response has no sessionHandle".into())
                    .into()
            })
    }

    async fn submit(&self, session: &str, sql: &str) -> Result<String, FlowError> {
        let req = self
            .client
            .post(format!(
                "{}/v1/sessions/{session}/statements",
                self.base_url
            ))
            .json(&serde_json::json!({ "statement": sql }));
        let body = self.send_json(req, "submit statement").await?;
        body.get("operationHandle")
            .and_then(|h| h.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                RdbmsError::Database("flink: submit response has no operationHandle".into()).into()
            })
    }

    async fn wait_finished(&self, session: &str, operation: &str) -> Result<(), FlowError> {
        loop {
            let url = format!(
                "{}/v1/sessions/{session}/operations/{operation}/status",
                self.base_url
            );
            let body = self
                .send_json(self.client.get(url), "operation status")
                .await?;
            match body.get("status").and_then(|s| s.as_str()).unwrap_or("") {
                "FINISHED" => return Ok(()),
                status @ ("ERROR" | "CANCELED") => {
                    // 官方状态体只有 `status`；部分版本/代理附 `error`，有就带上。
                    let detail = body.get("error").and_then(|e| e.as_str()).unwrap_or("");
                    let msg = if detail.is_empty() {
                        format!("flink: operation ended with status {status}")
                    } else {
                        format!("flink: operation ended with status {status}: {detail}")
                    };
                    return Err(RdbmsError::Database(msg).into());
                }
                "INITIALIZED" | "RUNNING" => tokio::time::sleep(POLL_INTERVAL).await,
                other => {
                    return Err(RdbmsError::Database(format!(
                        "flink: unexpected operation status {other:?}"
                    ))
                    .into());
                }
            }
        }
    }

    /// 从 `result/0` 起拉取并聚合所有分页。
    async fn fetch_results(&self, session: &str, operation: &str) -> Result<Vec<Row>, FlowError> {
        let mut url = format!(
            "{}/v1/sessions/{session}/operations/{operation}/result/0?rowFormat=JSON",
            self.base_url
        );
        let mut rows = Vec::new();
        loop {
            let body = self
                .send_json(self.client.get(&url), "fetch results")
                .await?;
            // 正常流程先轮询到 FINISHED 才拉结果；NOT_READY 仍按官方语义重试。
            if body.get("resultType").and_then(|t| t.as_str()) == Some("NOT_READY") {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            let results = body.get("results");
            let columns: Vec<String> = results
                .and_then(|r| r.get("columns"))
                .and_then(|c| c.as_array())
                .map(|cols| {
                    cols.iter()
                        .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(data) = results
                .and_then(|r| r.get("data"))
                .and_then(|d| d.as_array())
            {
                for row in data {
                    let fields = row
                        .get("fields")
                        .and_then(|f| f.as_array())
                        .ok_or_else(|| {
                            RdbmsError::Database(
                                "flink parse results: row has no fields array".into(),
                            )
                        })?;
                    if fields.len() != columns.len() {
                        return Err(RdbmsError::Database(format!(
                            "flink parse results: row has {} fields, expected {}",
                            fields.len(),
                            columns.len()
                        ))
                        .into());
                    }
                    rows.push(Row::new(columns.clone(), fields.clone()));
                }
            }
            match body.get("nextResultUri").and_then(|u| u.as_str()) {
                Some(next) => url = next_url(&self.base_url, next),
                None => return Ok(rows),
            }
        }
    }
}

/// `nextResultUri` 官方给的是相对路径（`/v1/sessions/.../result/1`），也兼容绝对 URL。
fn next_url(base_url: &str, next: &str) -> String {
    if next.starts_with("http://") || next.starts_with("https://") {
        next.to_string()
    } else {
        format!("{base_url}{next}")
    }
}

#[async_trait]
impl SqlExecutor for FlinkClient {
    /// 提交并等待操作结束。Flink SQL Gateway 不提供可靠的 DML 影响行数
    /// （异步操作只报状态），因此恒返回 0。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.guarded(self.flow(sql, false)).await.map(|_| 0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.guarded(self.flow(sql, true)).await
    }

    /// 本 client 不支持参数绑定，按文档契约回退到 [`Dialect::Standard`]。
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for FlinkClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "Flink SQL Gateway does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests;

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Trino 客户端（client REST protocol：`POST /v1/statement` + `nextUri` 轮询）。
//!
//! 协议依据 <https://trino.io/docs/current/develop/client-protocol.html>：
//! - 起始 `POST /v1/statement`，body 是 SQL 文本，`X-Trino-User` 必需；
//!   `X-Trino-Catalog` / `X-Trino-Schema` / `X-Trino-Session` 可选。
//! - 响应带 `nextUri` 就 `GET` 它，直到没有 `nextUri`；后续页可能省略
//!   `columns`，列名沿用上一页。
//! - `error` 对象存在即失败（HTTP 状态仍可能是 200），`message` 映射为
//!   [`RdbmsError::Database`]。
//!
//! 轮询循环整体包在**一次** `guarded` 里：`query_timeout_secs` 覆盖整条查询，
//! 而不是单页。

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
use tokio::sync::{Semaphore, SemaphorePermit};

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

/// `X-Trino-User` 未配置时的默认会话用户。
const DEFAULT_USER: &str = "ecat";
/// `X-Trino-Source`：Trino 查询详情里标识提交方软件。
const SOURCE: &str = "ecat";

#[derive(Debug, Clone, Deserialize)]
pub struct TrinoConfig {
    pub base_url: String,
    /// 会话用户（`X-Trino-User`）；未配置 = `"ecat"`。
    #[serde(default)]
    pub user: Option<String>,
    /// 配置后启用 HTTP Basic 认证。
    #[serde(default)]
    pub password: Option<String>,
    /// 默认 catalog（`X-Trino-Catalog`）。
    #[serde(default)]
    pub catalog: Option<String>,
    /// 默认 schema（`X-Trino-Schema`）。
    #[serde(default)]
    pub schema: Option<String>,
    /// 会话属性，拼成 `X-Trino-Session: k=v,k=v`。
    #[serde(default)]
    pub session_properties: HashMap<String, String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 整条查询的超时秒数（含所有 `nextUri` 分页）。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒、`new` / `with_auth` 没有）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = 不限并发（与 `query_timeout_secs: 0` = 禁用同构）；
    /// 未配置 = 32。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct TrinoClient {
    client: reqwest::Client,
    base_url: String,
    user: String,
    password: Option<String>,
    catalog: Option<String>,
    schema: Option<String>,
    session_properties: HashMap<String, String>,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
}

/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

/// `error` 对象存在即失败；消息取 `message`，缺该字段时退回整个对象文本。
fn error_message(body: &serde_json::Value) -> Option<String> {
    let err = body.get("error").filter(|e| !e.is_null())?;
    Some(
        err.get("message")
            .and_then(|m| m.as_str())
            .map_or_else(|| err.to_string(), str::to_string),
    )
}

impl TrinoClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            user: DEFAULT_USER.into(),
            password: None,
            catalog: None,
            schema: None,
            session_properties: HashMap::new(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            user: user.into(),
            password: Some(password.into()),
            catalog: None,
            schema: None,
            session_properties: HashMap::new(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn from_config(cfg: TrinoConfig) -> Result<Self, RdbmsError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| RdbmsError::Config(format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // `metrics` feature 下构造即注册（幂等；多 client 不会炸）。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            user: cfg.user.unwrap_or_else(|| DEFAULT_USER.into()),
            password: cfg.password,
            catalog: cfg.catalog,
            schema: cfg.schema,
            session_properties: cfg.session_properties,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
            semaphore: match cfg.max_concurrency {
                // `0` = 不限并发：不建信号量。建 `Semaphore::new(0)` 会让每次
                // 调用静默无限挂起（`guarded` 第一句就是 `permit().await`）。
                Some(0) => None,
                Some(n) => Some(Arc::new(Semaphore::new(n))),
                None => Some(Arc::new(Semaphore::new(32))),
            },
        })
    }

    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
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

    /// 一次出站调用的公共外壳：**许可 → 熔断 → 超时**。
    ///
    /// 这个顺序不能改：**超时若在外层，熔断器会对卡死的后端永久失明** ——
    /// 超时触发时 `tokio::time::timeout` 会 drop 内层 future，而熔断器记失败的
    /// 那句在 `f().await` **之后**，于是窗口里什么都不记，熔断器永远不打开。
    /// 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断。
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

    /// 起始请求：SQL 文本 POST 到 `/v1/statement`。
    ///
    /// `X-Trino-*` 头只在起始 POST 上发（协议规定后续 `GET nextUri` 不需要）。
    fn start_request(&self, sql: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(format!("{}/v1/statement", self.base_url))
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("X-Trino-User", self.user.as_str())
            .header("X-Trino-Source", SOURCE)
            .body(sql.to_string());
        if let Some(catalog) = &self.catalog {
            req = req.header("X-Trino-Catalog", catalog.as_str());
        }
        if let Some(schema) = &self.schema {
            req = req.header("X-Trino-Schema", schema.as_str());
        }
        if !self.session_properties.is_empty() {
            // 排序只为配置确定性；Trino 不要求会话属性顺序。
            let mut props: Vec<(&String, &String)> = self.session_properties.iter().collect();
            props.sort();
            let value = props
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            req = req.header("X-Trino-Session", value);
        }
        req
    }

    /// Basic 认证要发在**每个**请求上（含 `GET nextUri`）。
    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.password {
            Some(password) => req.basic_auth(self.user.as_str(), Some(password)),
            None => req,
        }
    }

    /// 发一个协议请求：非 2xx 失败（body 原样带回），2xx 解析 JSON。
    async fn fetch(&self, req: reqwest::RequestBuilder) -> Result<serde_json::Value, RdbmsError> {
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("trino: {e}")))?;
        if !resp.status().is_success() {
            return Err(RdbmsError::Database(
                resp.text().await.unwrap_or_else(|e| format!("trino: {e}")),
            ));
        }
        resp.json()
            .await
            .map_err(|e| RdbmsError::Database(format!("trino parse: {e}")))
    }

    /// 跑完整条查询：起始页 + 所有 `nextUri` 分页。返回
    /// `(stats.processedRows, rows)`。
    ///
    /// 整个循环在一次 `guarded` 里，所以超时与熔断都按**整条查询**计。
    async fn run(&self, sql: &str) -> Result<(u64, Vec<Row>), RdbmsError> {
        let start = self.start_request(sql);
        self.guarded(async move {
            let mut body = self.fetch(start).await?;
            // 后续页可能省略 `columns`，沿用上一页的列名。
            let mut columns: Option<Vec<String>> = None;
            let mut rows = Vec::new();
            let mut processed = 0;
            loop {
                if let Some(message) = error_message(&body) {
                    return Err(RdbmsError::Database(message));
                }
                if let Some(cols) = body.get("columns").and_then(|c| c.as_array()) {
                    columns = Some(
                        cols.iter()
                            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                            .map(str::to_string)
                            .collect(),
                    );
                }
                if let Some(data) = body.get("data").and_then(|d| d.as_array())
                    && let Some(cols) = &columns
                {
                    for row in data {
                        if let Some(values) = row.as_array() {
                            rows.push(Row::new(cols.clone(), values.clone()));
                        }
                    }
                }
                if let Some(n) = body
                    .get("stats")
                    .and_then(|s| s.get("processedRows"))
                    .and_then(serde_json::Value::as_u64)
                {
                    // `stats` 是累计值；最后一页即整条语句的总数。
                    processed = n;
                }
                let Some(next) = body.get("nextUri").and_then(|n| n.as_str()) else {
                    return Ok((processed, rows));
                };
                body = self.fetch(self.client.get(next)).await?;
            }
        })
        .await
    }
}

#[async_trait]
impl SqlExecutor for TrinoClient {
    /// 返回 `stats.processedRows`（缺字段按 0）。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.run(sql).await.map(|(processed, _)| processed)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.run(sql).await.map(|(_, rows)| rows)
    }

    /// Trino 的 SQL 方言近似 ANSI 但 `Dialect` 无对应变体；本 client 也不支持
    /// 参数绑定，按文档契约回退到 [`Dialect::Standard`]。
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for TrinoClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "Trino client does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests;

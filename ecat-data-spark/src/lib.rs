// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Spark SQL over the Apache Livy REST API.
//!
//! Reference: <https://livy.apache.org/docs/latest/rest-api.html> (checked 2026-10-29).
//!
//! # Flow
//!
//! 1. `POST /sessions` with `{"kind":"sql","conf":{…}}`; poll
//!    `GET /sessions/{id}` until `state == "idle"`. `dead` / `error` / `killed`
//!    fail the call and drop the cached session.
//! 2. `POST /sessions/{id}/statements` with `{"code": sql}`; poll
//!    `GET /sessions/{id}/statements/{stmt}` until `state == "available"`.
//! 3. Rows come from `output.data["application/json"]` (a JSON-encoded array
//!    of row objects, or the array itself) or `output.data["text/plain"]`.
//!
//! # Concurrency limit
//!
//! **Statements on one `SparkClient` are serialized.** Livy runs an interactive
//! session's statements one at a time in submission order and only keeps the
//! latest statement's output, so interleaving a second "submit + poll" cycle
//! would let one query observe the other's rows. One mutex guards the cached
//! session id *and* is held across the whole submit+poll cycle; a second caller
//! waits. `max_concurrency` still caps outer calls (permit before the breaker),
//! but the session mutex makes this client effectively single-statement.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, map_breaker_error,
    run_with_timeout,
};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore, SemaphorePermit};

mod protocol;
use protocol::{EntityId, SessionState, Statement};

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

/// Session/statement state poll interval. Interactive Spark statements take
/// seconds, so 100 ms gives prompt pickup without hammering Livy.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Deserialize)]
pub struct SparkConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Spark config properties passed as `conf` on session creation.
    #[serde(default)]
    pub session_conf: HashMap<String, String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算（包住整个「建 session + 提交 + 轮询」流程），与 reqwest
    /// 自带的总超时（`from_config` 建的 client 有 30 秒、`new` / `with_auth`
    /// 没有）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    ///
    /// 熔断**默认开启**。`BreakerConfig` 没有 `enabled` 字段，写
    /// `{"enabled": false}` 只会反序列化失败；要停用只能把阈值调到不可能触发。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = **不限并发**；未配置 = 32。
    ///
    /// 注意：本 client 的 statement 串行互斥使实际并发上限为 1（见模块文档），
    /// 这里的许可仍是外层背压（例如熔断打开时的快速失败路径）。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct SparkClient {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    session_conf: HashMap<String, String>,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
    /// 缓存的 Livy session id；这把锁同时串行化整个 statement 周期。
    session: Mutex<Option<i64>>,
}

/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

impl SparkClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_auth_opt(base_url, None, None)
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self::with_auth_opt(base_url, Some(username.into()), Some(password.into()))
    }

    fn with_auth_opt(
        base_url: impl Into<String>,
        username: Option<String>,
        password: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            username,
            password,
            session_conf: HashMap::new(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
            session: Mutex::new(None),
        }
    }

    pub fn from_config(cfg: SparkConfig) -> Result<Self, RdbmsError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| RdbmsError::Config(format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 构造即注册（与其余后端一致）；`register_outbound_metrics` 幂等。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            username: cfg.username,
            password: cfg.password,
            session_conf: cfg.session_conf,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
            semaphore: match cfg.max_concurrency {
                // `0` = 不限并发：不建信号量。`Semaphore::new(0)` 会让每次调用
                // 在 `permit().await` 上静默无限挂起（超时层在它里面）。
                Some(0) => None,
                Some(n) => Some(Arc::new(Semaphore::new(n))),
                None => Some(Arc::new(Semaphore::new(32))),
            },
            session: Mutex::new(None),
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
    /// 顺序不能改：超时若在外层，熔断器会对卡死的后端永久失明（超时 drop 内层
    /// future，熔断记失败的那句在其后）。许可在最外：排队中的请求还没碰后端，
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

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// 发请求并解出 JSON；非 2xx 把服务端原始 body 带进错误（认证走
    /// Authorization 头、不在 URL 里，所以错误文本不会泄凭据）。
    async fn send(
        &self,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<serde_json::Value, RdbmsError> {
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("spark: {what}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RdbmsError::Database(format!(
                "spark: {what}: HTTP {status}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| RdbmsError::Database(format!("spark: {what}: parse: {e}")))
    }

    async fn create_session(&self) -> Result<i64, RdbmsError> {
        let body = serde_json::json!({ "kind": "sql", "conf": self.session_conf });
        let value = self
            .send(
                self.client.post(self.url("/sessions")).json(&body),
                "create session",
            )
            .await?;
        let created: EntityId = serde_json::from_value(value)
            .map_err(|e| RdbmsError::Database(format!("spark: create session: parse: {e}")))?;
        Ok(created.id)
    }

    /// 轮询会话直到 `idle`。终态 `dead` / `error` / `killed` / … 清空缓存并报错；
    /// `not_started` / `starting` / `busy` 继续等（`busy` = 上一条语句还在跑）。
    async fn wait_idle(&self, slot: &mut Option<i64>) -> Result<(), RdbmsError> {
        let id = slot.expect("caller set the session id");
        loop {
            let value = self
                .send(
                    self.client.get(self.url(&format!("/sessions/{id}"))),
                    "session state",
                )
                .await?;
            let state: SessionState = serde_json::from_value(value)
                .map_err(|e| RdbmsError::Database(format!("spark: session state: parse: {e}")))?;
            match state.state.as_str() {
                "idle" => return Ok(()),
                "not_started" | "starting" | "busy" => {}
                "dead" | "error" | "killed" | "shutting_down" | "success" => {
                    *slot = None;
                    return Err(RdbmsError::Database(format!(
                        "spark: session {id} is {}",
                        state.state
                    )));
                }
                other => {
                    return Err(RdbmsError::Database(format!(
                        "spark: session {id} entered unexpected state {other:?}"
                    )));
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// 整条语句流程；调用方保证它被 [`SparkClient::guarded`] 包住。
    ///
    /// 从取 session 到 statement 结束一直持锁 —— 见模块文档的并发限制。
    async fn run_statement(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let mut slot = self.session.lock().await;
        if slot.is_none() {
            *slot = Some(self.create_session().await?);
        }
        self.wait_idle(&mut slot).await?;
        let session_id = slot.expect("session id set above");

        let body = serde_json::json!({ "code": sql });
        let value = self
            .send(
                self.client
                    .post(self.url(&format!("/sessions/{session_id}/statements")))
                    .json(&body),
                "submit statement",
            )
            .await?;
        let submitted: EntityId = serde_json::from_value(value)
            .map_err(|e| RdbmsError::Database(format!("spark: submit statement: parse: {e}")))?;

        loop {
            let value = self
                .send(
                    self.client.get(self.url(&format!(
                        "/sessions/{session_id}/statements/{}",
                        submitted.id
                    ))),
                    "statement state",
                )
                .await?;
            let statement: Statement = serde_json::from_value(value)
                .map_err(|e| RdbmsError::Database(format!("spark: statement: parse: {e}")))?;
            match statement.phase()? {
                Some(rows) => return Ok(rows),
                None => tokio::time::sleep(POLL_INTERVAL).await,
            }
        }
    }

    async fn run(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        // 整条流程（建 session + 提交 + 轮询）算**一次**出站调用：一次许可、
        // 一次熔断记录、一个总超时预算。
        self.guarded(self.run_statement(sql)).await
    }
}

#[async_trait]
impl SqlExecutor for SparkClient {
    /// Livy SQL does not report a reliable affected-row count (Spark returns rows,
    /// not `num_affected_rows`), so this always returns 0.
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.run(sql).await?;
        Ok(0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.run(sql).await
    }

    /// Spark SQL is close to ANSI SQL; `Dialect` has no Spark variant, so fall
    /// back to [`Dialect::Standard`] (same choice as the QuestDB client).
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for SparkClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "Spark does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests;

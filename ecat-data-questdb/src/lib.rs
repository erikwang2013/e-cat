// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! QuestDB client (HTTP `/exec` endpoint).
//!
//! Error responses pass through the server's raw body text: credentials are
//! sent via the Authorization header (never in the URL), so error messages
//! cannot leak secrets; outer layers handle the generic error text.

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, map_breaker_error,
    run_with_timeout,
};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

#[derive(Debug, Clone, Deserialize)]
pub struct QuestdbConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒、`new` / `with_auth` 没有）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    ///
    /// 熔断**默认开启** —— 保守阈值下只在持续失败时打开。**当前没有总开关**：
    /// `BreakerConfig` 只有阈值字段，没有 `enabled`（不许写 `{"enabled": false}`：
    /// 那是反序列化错误，或被 `#[serde(default)]` 静默吞掉后以为关掉了）。
    /// 真要停用，只能把阈值调到不可能触发（如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = **不限并发**（与 `query_timeout_secs: 0` = 禁用同构）；
    /// 未配置 = 32。
    ///
    /// reqwest **只有** `pool_max_idle_per_host`（空闲保留数），没有「最大总连接数」
    /// —— 默认无上限意味着并发无背压。上限由本 crate 的信号量实现，不是 reqwest 的旋钮。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct QuestdbClient {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
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

impl QuestdbClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            username: None,
            password: None,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            username: Some(username.into()),
            password: Some(password.into()),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn from_config(cfg: QuestdbConfig) -> Result<Self, RdbmsError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| RdbmsError::Config(format!("TLS: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: match cfg.max_concurrency {
                // `0` = 不限并发（与 `query_timeout_secs: 0` = 禁用同构）：
                // 不建信号量。建 `Semaphore::new(0)` 会让每次调用静默无限挂起
                // —— `guarded` 的第一句就是 `permit().await`，超时层在它里面。
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
    /// 超时触发时 `tokio::time::timeout` 会 drop 内层 future，而熔断器记失败的那句
    /// 在 `f().await` **之后**，于是每次都只留下一次 drop、窗口里什么都不记，
    /// 熔断器永远不打开（这正是本设计要防的头号场景）。详见批次 5a 计划的「出入 4」。
    ///
    /// 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断。
    ///
    /// 维度**写死**不收参数：本 crate 只有 `SqlExecutor` 一条 I/O 路径
    /// （ClickHouse 收参数是因为它有两条路径共用外壳）。多一个永不变化的入参就多一个
    /// 填错的机会，而填错只是静默少数（`ecat-data/src/timeout.rs:15-35`），没有编译期保护。
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
}

#[async_trait]
impl SqlExecutor for QuestdbClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        // 请求构造（含 URL/头/体）是纯本地的，留在壳外；只有 `send()` 起才是 I/O。
        let req = self
            .client
            .post(format!("{}/exec", self.base_url))
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(sql.to_string());
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("questdb: {e}")))?;
            if !resp.status().is_success() {
                return Err(RdbmsError::Database(
                    resp.text()
                        .await
                        .unwrap_or_else(|e| format!("questdb: {e}")),
                ));
            }
            Ok(0)
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let req = self
            .client
            .post(format!("{}/exec?count=true", self.base_url))
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("Accept", "application/json")
            .body(sql.to_string());
        // 请求构造（含 URL/头/体）是纯本地的，留在壳外；只有 `send()` 起才是 I/O。
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("questdb: {e}")))?;
            if !resp.status().is_success() {
                return Err(RdbmsError::Database(
                    resp.text()
                        .await
                        .unwrap_or_else(|e| format!("questdb: {e}")),
                ));
            }
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| RdbmsError::Database(format!("questdb parse: {e}")))?;
            // 2xx 响应也可能携带 error 字段（无 columns/dataset 时）
            if let Some(err) = body
                .get("error")
                .and_then(|e| e.as_str())
                .filter(|e| !e.is_empty())
            {
                return Err(RdbmsError::Database(err.to_string()));
            }
            let mut rows = Vec::new();
            if let Some(columns) = body.get("columns").and_then(|c| c.as_array()) {
                let cols: Vec<String> = columns
                    .iter()
                    .filter_map(|c| {
                        c.get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                if let Some(dataset) = body.get("dataset").and_then(|d| d.as_array()) {
                    for row in dataset {
                        if let Some(vals) = row.as_array() {
                            rows.push(Row::new(cols.clone(), vals.clone()));
                        }
                    }
                }
            }
            Ok(rows)
        })
        .await
    }

    /// QuestDB 走 HTTP `/exec`，SQL 为 Postgres 味但 `Dialect` 无对应变体，
    /// 且本 client 不支持参数绑定，按文档契约回退到 [`Dialect::Standard`]。
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for QuestdbClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "QuestDB does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    mod resilience;
    use super::*;

    #[test]
    fn client_constructs() {
        let _client = QuestdbClient::new("http://localhost:9000");
    }

    #[test]
    fn config_with_optional_auth() {
        let cfg: QuestdbConfig = serde_json::from_str(
            r#"{"base_url":"http://localhost:9000","username":"admin","password":"quest"}"#,
        )
        .unwrap();
        let client = QuestdbClient::from_config(cfg).unwrap();
        assert!(client.username.is_some());
    }

    /// mock QuestDB 的 /exec 端点，返回给定状态码与响应体。
    async fn spawn_mock_exec(status: u16, body: &'static str) -> String {
        let app = axum::Router::new().route(
            "/exec",
            axum::routing::post(move || async move {
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    axum::response::Response::new(axum::body::Body::from(body)),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn query_returns_err_on_http_400() {
        let base_url =
            spawn_mock_exec(400, r#"{"code":"invalid","error":"table not found"}"#).await;
        let client = QuestdbClient::new(base_url);
        let err = client.query("select * from nope").await.unwrap_err();
        assert!(err.to_string().contains("table not found"));
    }

    #[tokio::test]
    async fn query_returns_err_on_2xx_with_error_field() {
        let base_url = spawn_mock_exec(200, r#"{"error":"no columns"}"#).await;
        let client = QuestdbClient::new(base_url);
        let err = client.query("select 1").await.unwrap_err();
        assert!(err.to_string().contains("no columns"));
    }

    #[tokio::test]
    async fn query_parses_dataset_into_rows() {
        let body = r#"{
            "columns": [{"name": "id", "type": "INT"}, {"name": "name", "type": "STRING"}],
            "dataset": [[1, "alice"], [2, "bob"]],
            "count": 2
        }"#;
        let base_url = spawn_mock_exec(200, body).await;
        let client = QuestdbClient::new(base_url);
        let rows = client.query("select * from t").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("id"), Some(&serde_json::json!(1)));
        assert_eq!(rows[0].get("name"), Some(&serde_json::json!("alice")));
        assert_eq!(rows[1].get("id"), Some(&serde_json::json!(2)));
    }

    #[tokio::test]
    async fn query_handles_empty_dataset() {
        let body = r#"{"columns": [{"name": "id", "type": "INT"}], "dataset": [], "count": 0}"#;
        let base_url = spawn_mock_exec(200, body).await;
        let client = QuestdbClient::new(base_url);
        let rows = client.query("select * from t").await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn query_skips_malformed_dataset_rows() {
        // 行不是数组时跳过，不 panic
        let body = r#"{"columns": [{"name": "id", "type": "INT"}], "dataset": [[1], "oops", [3]]}"#;
        let base_url = spawn_mock_exec(200, body).await;
        let client = QuestdbClient::new(base_url);
        let rows = client.query("select * from t").await.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn execute_returns_zero_on_success() {
        let base_url = spawn_mock_exec(200, r#"{"ddl":"OK"}"#).await;
        let client = QuestdbClient::new(base_url);
        assert_eq!(client.execute("create table t (i int)").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn transaction_returns_not_supported_error() {
        let client = QuestdbClient::new("http://localhost:9000");
        let err = match client.transaction().await {
            Err(e) => e,
            Ok(_) => panic!("expected unsupported error"),
        };
        assert!(
            err.to_string().contains("does not support transactions"),
            "got: {err}"
        );
    }
}

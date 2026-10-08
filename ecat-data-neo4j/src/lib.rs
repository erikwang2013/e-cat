// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, GraphClient, breaker_error_to_backend_error, run_with_timeout};
use ecat_errors::{Error, ErrorCode};
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
pub struct Neo4jConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
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

pub struct Neo4jClient {
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
}

impl Neo4jClient {
    pub fn new(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            username: username.into(),
            password: password.into(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn from_config(cfg: Neo4jConfig) -> Result<Self, Error> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "neo4j_tls", format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线（lead 裁决 2026-10-08）：`metrics` feature 下**构造即注册**，
        // 用户代码零变化。`ecat_metrics::register_outbound_metrics` 是幂等的
        // （同一 backend 重复注册是覆盖闭包），所以多 client 不会炸。
        // 探针：注释掉下面两行 ⇒ from_config_registers_outbound_metrics 红。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
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
    /// 顺序见 T0 模板（超时若在外层，熔断器会对卡死的后端永久失明）。
    /// 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断。
    ///
    /// `kind` **写死**不收参数：本 crate 只有一条 I/O 路径（ClickHouse 收参数是因为
    /// 它有两条路径共用外壳）。多一个永不变化的入参就多一个填错的机会，
    /// 而填错只是静默少数（`ecat-data/src/timeout.rs:15-35`），没有编译期保护。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Graph, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "neo4j"))
    }
}

/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

#[async_trait]
impl GraphClient for Neo4jClient {
    async fn execute(
        &self,
        cypher: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let body = serde_json::json!({"statements": [{"statement": cypher, "parameters": params}]});
        self.guarded(async {
            let resp = self
                .client
                .post(format!("{}/db/data/transaction/commit", self.base_url))
                .basic_auth(&self.username, Some(&self.password))
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "neo4j", format!("neo4j: {e}")))?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "neo4j",
                    resp.text().await.unwrap_or_default(),
                ));
            }
            resp.json()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "neo4j", format!("neo4j parse: {e}")))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    mod resilience;

    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct CapturedRequest {
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    type MockState = (Arc<Mutex<Vec<CapturedRequest>>>, u16, &'static str);

    /// mock Neo4j transaction-commit 端点：捕获请求路径/头/体，按给定
    /// 状态码与响应体应答（body 为空时返回成功 JSON），返回 mock base_url。
    async fn spawn_mock(
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        status: u16,
        body: &'static str,
    ) -> String {
        let app = Router::new()
            .route("/db/data/transaction/commit", post(handle))
            .with_state((captured, status, body));

        async fn handle(
            State((captured, status, body)): State<MockState>,
            req: axum::http::Request<Body>,
        ) -> axum::response::Response {
            let path = req.uri().path().to_string();
            let (parts, req_body) = req.into_parts();
            let headers = parts
                .headers
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        v.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            let req_body = to_bytes(req_body, usize::MAX).await.unwrap_or_default();
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(CapturedRequest {
                    path,
                    headers,
                    body: req_body.to_vec(),
                });
            if body.is_empty() {
                Json(serde_json::json!({"results": [], "errors": []})).into_response()
            } else {
                (StatusCode::from_u16(status).unwrap(), Body::from(body)).into_response()
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn execute_builds_correct_request() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 200, "").await;
        let client = Neo4jClient::new(base_url, "neo4j", "secret");
        let query = "MATCH (n:User) RETURN n LIMIT $limit";
        let params = serde_json::json!({"limit": 10});
        client.execute(query, &params).await.unwrap();

        let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reqs.len(), 1);
        let r = &reqs[0];
        assert_eq!(r.path, "/db/data/transaction/commit");
        // base64("neo4j:secret")
        assert_eq!(r.header("authorization"), Some("Basic bmVvNGo6c2VjcmV0"));
        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(body["statements"][0]["statement"], query);
        assert_eq!(body["statements"][0]["parameters"]["limit"], 10);
    }

    #[tokio::test]
    async fn execute_propagates_server_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 500, "boom").await;
        let client = Neo4jClient::new(base_url, "neo4j", "secret");
        let err = client
            .execute("MATCH (n) RETURN n", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("boom"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_404_returns_body_as_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 404, "not found").await;
        let client = Neo4jClient::new(base_url, "neo4j", "secret");
        let err = client
            .execute("MATCH (n) RETURN n", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_non_json_body_returns_parse_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 200, "definitely not json").await;
        let client = Neo4jClient::new(base_url, "neo4j", "secret");
        let err = client
            .execute("MATCH (n) RETURN n", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("neo4j parse"), "got: {err}");
    }
}

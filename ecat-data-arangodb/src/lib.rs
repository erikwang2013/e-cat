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
pub struct ArangoConfig {
    pub base_url: String,
    pub db: String,
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
    /// 并发上限。reqwest **只有** `pool_max_idle_per_host`（空闲保留数），
    /// 没有「最大总连接数」—— 默认无上限意味着并发无背压。
    /// 未配置 = 32。上限由本 crate 的信号量实现，不是 reqwest 的旋钮。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct ArangoClient {
    client: reqwest::Client,
    base_url: String,
    db: String,
    username: String,
    password: String,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    semaphore: Arc<Semaphore>,
}

impl ArangoClient {
    pub fn new(
        base_url: impl Into<String>,
        db: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            db: db.into(),
            username: username.into(),
            password: password.into(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Arc::new(Semaphore::new(32)),
        }
    }

    pub fn from_config(cfg: ArangoConfig) -> Result<Self, Error> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "arango_tls", format!("TLS: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
            db: cfg.db,
            username: cfg.username,
            password: cfg.password,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32))),
        })
    }

    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 取一个并发许可。信号量从不 `close()`，`AcquireError` 不可达。
    async fn permit(&self) -> SemaphorePermit<'_> {
        self.semaphore
            .acquire()
            .await
            .expect("semaphore is never closed")
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
            .map_err(|e| breaker_error_to_backend_error(e, "arangodb"))
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

/// Percent-encode a single URL path segment (RFC 3986): every byte except
/// unreserved characters (`A-Z a-z 0-9 - _ . ~`) becomes `%XX`.
fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[async_trait]
impl GraphClient for ArangoClient {
    async fn execute(
        &self,
        aql: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let body = serde_json::json!({"query": aql, "bindVars": params});
        self.guarded(async {
            let resp = self
                .client
                .post(format!(
                    "{}/_db/{}/_api/cursor",
                    self.base_url,
                    percent_encode_segment(&self.db)
                ))
                .basic_auth(&self.username, Some(&self.password))
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "arango", format!("arango: {e}")))?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "arango",
                    resp.text().await.unwrap_or_default(),
                ));
            }
            resp.json().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "arango", format!("arango parse: {e}"))
            })
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

    /// mock ArangoDB cursor 端点（路径含编码后的 db 名，用 fallback 捕获
    /// 任意路径）：捕获请求路径/头/体，按给定状态码与响应体应答
    /// （body 为空时返回成功 JSON），返回 mock base_url。
    async fn spawn_mock(
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        status: u16,
        body: &'static str,
    ) -> String {
        let app = Router::new()
            .fallback(handle)
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
                Json(serde_json::json!({"result": []})).into_response()
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

    #[test]
    fn percent_encode_segment_encodes_reserved_chars() {
        assert_eq!(percent_encode_segment("mydb"), "mydb");
        assert_eq!(percent_encode_segment("my db/1"), "my%20db%2F1");
        assert_eq!(percent_encode_segment("你好"), "%E4%BD%A0%E5%A5%BD");
    }

    #[test]
    fn config_deserializes() {
        let cfg: ArangoConfig = serde_json::from_value(serde_json::json!({
            "base_url": "http://localhost:8529",
            "db": "mydb",
            "username": "root",
            "password": "secret",
        }))
        .unwrap();
        assert_eq!(cfg.db, "mydb");
        assert!(cfg.tls.is_none());
    }

    #[test]
    fn config_missing_db_is_error() {
        let result: Result<ArangoConfig, _> = serde_json::from_str(
            r#"{"base_url":"http://localhost:8529","username":"root","password":"s"}"#,
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_builds_correct_request() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 200, "").await;
        let client = ArangoClient::new(base_url, "mydb", "root", "secret");
        let aql = "FOR u IN users FILTER u.age > @min RETURN u";
        let params = serde_json::json!({"min": 18});
        client.execute(aql, &params).await.unwrap();

        let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reqs.len(), 1);
        let r = &reqs[0];
        assert_eq!(r.path, "/_db/mydb/_api/cursor");
        // base64("root:secret")
        assert_eq!(r.header("authorization"), Some("Basic cm9vdDpzZWNyZXQ="));
        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(body["query"], aql);
        assert_eq!(body["bindVars"]["min"], 18);
    }

    #[tokio::test]
    async fn execute_percent_encodes_db_name() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 200, "").await;
        let client = ArangoClient::new(base_url, "my db/1", "root", "");
        client
            .execute("RETURN 1", &serde_json::json!({}))
            .await
            .unwrap();

        let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/_db/my%20db%2F1/_api/cursor");
    }

    #[tokio::test]
    async fn execute_propagates_server_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 500, "boom").await;
        let client = ArangoClient::new(base_url, "mydb", "root", "");
        let err = client
            .execute("RETURN 1", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("boom"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_non_json_body_returns_parse_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 200, "not json").await;
        let client = ArangoClient::new(base_url, "mydb", "root", "");
        let err = client
            .execute("RETURN 1", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("arango parse"), "got: {err}");
    }
}

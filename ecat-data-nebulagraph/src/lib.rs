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
pub struct NebulaGraphConfig {
    pub base_url: String,
    pub space: String,
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

pub struct NebulaGraphClient {
    client: reqwest::Client,
    base_url: String,
    space: String,
    username: Option<String>,
    password: Option<String>,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
}

impl NebulaGraphClient {
    pub fn new(base_url: impl Into<String>, space: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            space: space.into(),
            username: None,
            password: None,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        space: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            space: space.into(),
            username: Some(username.into()),
            password: Some(password.into()),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn from_config(cfg: NebulaGraphConfig) -> Result<Self, Error> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "nebula_tls", format!("TLS: {e}")))?;
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
            space: cfg.space,
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

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        ecat_tls::apply_basic_auth(req, &self.username, &self.password)
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
    /// **只包真正发 I/O 的路径。** 纯本地分支（本 crate 的 `params` 早退）必须留在
    /// 外面：半开态下每次 `call` 都要借走一个探测名额，而「没发请求就返回」会以 `Ok`
    /// 记账 —— 探测名额被白白消耗、熔断器还可能被**误关回 `Closed`**。记账口径是
    /// 「后端的表现」，不是「函数的返回值」。（测试
    /// `params_not_supported_does_not_touch_the_breaker` 盯着这条。）
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
            .map_err(|e| breaker_error_to_backend_error(e, "nebulagraph"))
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
impl GraphClient for NebulaGraphClient {
    async fn execute(
        &self,
        ngql: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        // 纯本地分支：**留在 `guarded` 外面**。半开态下每一次 `call` 都会借走一个
        // 探测名额，而「没发请求就返回」会以 `Ok` 记账 —— 名额被白白消耗，熔断器
        // 还可能被**误关回 `Closed`**。记账口径是「后端的表现」，不是「函数的返回值」。
        // （测试 `params_not_supported_does_not_touch_the_breaker` 盯着这条。）
        if !params.is_null() {
            return Err(Error::new(
                ErrorCode::Internal,
                "nebula",
                "params not supported",
            ));
        }
        // 请求构造（纯本地，不发 I/O）也可以留在外面；只有 `.send()` 那段进外壳。
        let req = self
            .client
            .post(format!("{}/api/ngql/execute", self.base_url))
            .json(&serde_json::json!({"gql": ngql, "space": self.space}));
        self.guarded(async {
            let resp =
                self.apply_auth(req).send().await.map_err(|e| {
                    Error::new(ErrorCode::Internal, "nebula", format!("nebula: {e}"))
                })?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "nebula",
                    resp.text().await.unwrap_or_default(),
                ));
            }
            resp.json().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "nebula", format!("nebula parse: {e}"))
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
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::{Arc, Mutex};

    #[test]
    fn client_constructs() {
        let _client = NebulaGraphClient::new("http://localhost:19669", "test_space");
    }

    #[test]
    fn config_with_optional_auth() {
        let cfg: NebulaGraphConfig = serde_json::from_str(
            r#"{"base_url":"http://localhost:19669","space":"test","username":"root","password":"nebula"}"#
        ).unwrap();
        let client = NebulaGraphClient::from_config(cfg).unwrap();
        assert!(client.username.is_some());
    }

    #[test]
    fn config_missing_space_is_error() {
        let result: Result<NebulaGraphConfig, _> =
            serde_json::from_str(r#"{"base_url":"http://localhost:19669"}"#);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_params() {
        let client = NebulaGraphClient::new("http://localhost:19669", "test_space");
        let err = client
            .execute("SHOW SPACES", &serde_json::json!({"limit": 5}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("params not supported"));
    }

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

    /// mock NebulaGraph ngql 端点：捕获请求路径/头/体，按给定状态码与
    /// 响应体应答（body 为空时返回成功 JSON），返回 mock base_url。
    async fn spawn_mock(
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        status: u16,
        body: &'static str,
    ) -> String {
        let app = Router::new()
            .route("/api/ngql/execute", post(handle))
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

    #[tokio::test]
    async fn execute_builds_correct_request() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 200, "").await;
        let client = NebulaGraphClient::with_auth(base_url, "test_space", "root", "nebula");
        let ngql = "SHOW SPACES";
        client
            .execute(ngql, &serde_json::Value::Null)
            .await
            .unwrap();

        let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reqs.len(), 1);
        let r = &reqs[0];
        assert_eq!(r.path, "/api/ngql/execute");
        // base64("root:nebula")
        assert_eq!(r.header("authorization"), Some("Basic cm9vdDpuZWJ1bGE="));
        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(body["gql"], ngql);
        assert_eq!(body["space"], "test_space");
    }

    #[tokio::test]
    async fn execute_propagates_server_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 500, "boom").await;
        let client = NebulaGraphClient::new(base_url, "test_space");
        let err = client
            .execute("SHOW SPACES", &serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("boom"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_non_json_body_returns_parse_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_mock(Arc::clone(&captured), 200, "not json at all").await;
        let client = NebulaGraphClient::new(base_url, "test_space");
        let err = client
            .execute("SHOW SPACES", &serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nebula parse"), "got: {err}");
    }
}

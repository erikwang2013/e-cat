// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Elasticsearch client.
//!
//! All writes/reads/errors are validated against the HTTP status code; index
//! names and document ids are percent-encoded before being placed in the URL
//! path so that reserved characters cannot break the request.

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, SearchClient, breaker_error_to_backend_error, run_with_timeout};
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
pub struct ElasticsearchConfig {
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

pub struct ElasticsearchClient {
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

impl ElasticsearchClient {
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

    pub fn from_config(cfg: ElasticsearchConfig) -> Result<Self, Error> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "es", format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线（lead 裁决 2026-10-08）：`metrics` feature 下**构造即注册**，
        // 用户代码零变化。`ecat_metrics::register_outbound_metrics` 幂等（同 backend
        // 重复注册 = 覆盖闭包）。探针：注释掉 `#[cfg(feature = "metrics")]` 那条语句
        // ⇒ `from_config_registers_outbound_metrics` 红（须用测试名过滤单独跑，否则
        // 同二进制的既有用例会掩盖）。上面那行是测试期锁（仅测试构建存在）：覆盖语义
        // 下观察者用例需要「观察窗内无别的写者」，见 `metrics.rs` 的 `TEST_SERIAL`。
        #[cfg(all(test, feature = "metrics"))]
        let _g = crate::metrics::lock_test_serial();
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
    /// 这个顺序不能改：**超时若在外层，熔断器会对卡死的后端永久失明** ——
    /// 超时触发时 `tokio::time::timeout` 会 drop 内层 future，而熔断器记失败的那句
    /// 在 `f().await` **之后**，于是每次都只留下一次 drop、窗口里什么都不记，
    /// 熔断器永远不打开（这正是本设计要防的头号场景）。详见批次 5a 计划的「出入 4」。
    ///
    /// 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断。
    ///
    /// `kind` **写死**不收参数：本 crate 的三个 I/O 方法同属 `SearchClient` 一个家族
    /// （ClickHouse 收参数是因为它有 `SqlExecutor` / `TsdbClient` 两条**不同家族**的路径
    /// 共用外壳）。多一个永不变化的入参就多一个填错的机会，
    /// 而填错只是静默少数（`ecat-data/src/timeout.rs:15-35`），没有编译期保护。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Search, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "elasticsearch"))
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        ecat_tls::apply_basic_auth(req, &self.username, &self.password)
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

/// Build the non-2xx error message, including the HTTP status code.
async fn status_error(prefix: &str, resp: reqwest::Response) -> Error {
    let status = resp.status().as_u16();
    Error::new(
        ErrorCode::Internal,
        "es",
        format!(
            "{prefix} failed: status {status}, body: {}",
            resp.text().await.unwrap_or_default()
        ),
    )
}

#[async_trait]
impl SearchClient for ElasticsearchClient {
    async fn index(&self, index: &str, id: &str, doc: &serde_json::Value) -> Result<(), Error> {
        let req = self
            .client
            .put(format!(
                "{}/{}/_doc/{}",
                self.base_url,
                percent_encode_segment(index),
                percent_encode_segment(id)
            ))
            .json(doc);
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "es", format!("es index: {e}")))?;
            if !resp.status().is_success() {
                return Err(status_error("es index", resp).await);
            }
            Ok(())
        })
        .await
    }

    async fn search(
        &self,
        index: &str,
        query: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let req = self
            .client
            .post(format!(
                "{}/{}/_search",
                self.base_url,
                percent_encode_segment(index)
            ))
            .json(query);
        self.guarded(async {
            let resp =
                self.apply_auth(req).send().await.map_err(|e| {
                    Error::new(ErrorCode::Internal, "es", format!("es search: {e}"))
                })?;
            if !resp.status().is_success() {
                return Err(status_error("es search", resp).await);
            }
            resp.json()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "es", format!("es parse: {e}")))
        })
        .await
    }

    async fn delete(&self, index: &str, id: &str) -> Result<(), Error> {
        let req = self.client.delete(format!(
            "{}/{}/_doc/{}",
            self.base_url,
            percent_encode_segment(index),
            percent_encode_segment(id)
        ));
        self.guarded(async {
            let resp =
                self.apply_auth(req).send().await.map_err(|e| {
                    Error::new(ErrorCode::Internal, "es", format!("es delete: {e}"))
                })?;
            if !resp.status().is_success() {
                return Err(status_error("es delete", resp).await);
            }
            Ok(())
        })
        .await
    }

    // `bulk_index` / `update` 走 `SearchClient` 的 trait 默认实现（`ecat-data/src/search.rs:17-34`），
    // **不包 `guarded`**：默认实现的「不支持」是**调用方的用法错**，不是后端故障。
    // 包了之后 5 次「不支持」就会打开熔断器，之后**正常检索全被拒绝**。守测试见
    // `tests/resilience.rs::unsupported_ops_do_not_trip_the_breaker`。
}

#[cfg(test)]
mod tests {
    mod resilience;

    use super::*;

    #[test]
    fn client_constructs() {
        let _client = ElasticsearchClient::new("http://localhost:9200");
    }

    #[test]
    fn client_with_auth() {
        let _client = ElasticsearchClient::with_auth("http://localhost:9200", "admin", "secret");
    }

    #[test]
    fn config_with_optional_auth() {
        let cfg: ElasticsearchConfig = serde_json::from_str(
            r#"{"base_url":"http://localhost:9200","username":"admin","password":"secret"}"#,
        )
        .unwrap();
        let client = ElasticsearchClient::from_config(cfg).unwrap();
        assert!(client.username.is_some());
    }

    #[test]
    fn config_without_auth() {
        let cfg: ElasticsearchConfig =
            serde_json::from_str(r#"{"base_url":"http://localhost:9200"}"#).unwrap();
        let client = ElasticsearchClient::from_config(cfg).unwrap();
        assert!(client.username.is_none());
    }

    #[test]
    fn percent_encode_segment_encodes_reserved_chars() {
        assert_eq!(percent_encode_segment("logs-2026"), "logs-2026");
        assert_eq!(
            percent_encode_segment("a/b c#d?e%f"),
            "a%2Fb%20c%23d%3Fe%25f"
        );
        assert_eq!(percent_encode_segment("你好"), "%E4%BD%A0%E5%A5%BD");
    }

    #[test]
    fn config_missing_base_url_is_error() {
        let result: Result<ElasticsearchConfig, _> = serde_json::from_str(r#"{}"#);
        assert!(result.is_err());
    }

    type Captured =
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, Vec<(String, String)>, Vec<u8>)>>>;

    /// mock Elasticsearch：捕获请求方法与路径，按给定状态码与响应体应答
    /// （body 为空时返回成功 JSON），返回 mock base_url。
    async fn spawn_mock(captured: Captured, status: u16, body: &'static str) -> String {
        let app = axum::Router::new().fallback(
            move |req: axum::http::Request<axum::body::Body>| async move {
                let method = req.method().to_string();
                let path = req.uri().path().to_string();
                let (parts, req_body) = req.into_parts();
                let headers: Vec<(String, String)> = parts
                    .headers
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.as_str().to_string(),
                            v.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                let req_body = axum::body::to_bytes(req_body, usize::MAX)
                    .await
                    .unwrap_or_default();
                captured.lock().unwrap_or_else(|e| e.into_inner()).push((
                    method,
                    path,
                    headers,
                    req_body.to_vec(),
                ));
                use axum::response::IntoResponse;
                if body.is_empty() {
                    axum::Json(serde_json::json!({"hits": {"total": 0}})).into_response()
                } else {
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        axum::response::Response::new(axum::body::Body::from(body)),
                    )
                        .into_response()
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[tokio::test]
    async fn index_puts_encoded_path_with_doc_and_auth() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let base_url = spawn_mock(captured.clone(), 200, "").await;
        let client = ElasticsearchClient::with_auth(base_url, "admin", "secret");
        client
            .index("logs 2026", "doc/1", &serde_json::json!({"msg": "hi"}))
            .await
            .unwrap();

        let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].0, "PUT");
        assert_eq!(reqs[0].1, "/logs%202026/_doc/doc%2F1");
        assert_eq!(
            header(&reqs[0].2, "authorization"),
            Some("Basic YWRtaW46c2VjcmV0"),
            "basic auth 头缺失"
        );
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].3).unwrap();
        assert_eq!(body["msg"], "hi");
    }

    #[tokio::test]
    async fn search_posts_query_and_parses_json_response() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let base_url = spawn_mock(captured.clone(), 200, r#"{"hits":{"total":2}}"#).await;
        let client = ElasticsearchClient::new(base_url);
        let query = serde_json::json!({"query": {"match_all": {}}});
        let v = client.search("idx", &query).await.unwrap();
        assert_eq!(v["hits"]["total"], 2);

        let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reqs[0].0, "POST");
        assert_eq!(reqs[0].1, "/idx/_search");
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].3).unwrap();
        assert_eq!(body, query);
    }

    #[tokio::test]
    async fn delete_sends_delete_request() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let base_url = spawn_mock(captured.clone(), 200, "").await;
        let client = ElasticsearchClient::new(base_url);
        client.delete("idx", "1").await.unwrap();
        let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reqs[0].0, "DELETE");
        assert_eq!(reqs[0].1, "/idx/_doc/1");
    }

    #[tokio::test]
    async fn index_propagates_http_error_with_status_and_body() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let base_url = spawn_mock(captured.clone(), 400, "mapper parse error").await;
        let client = ElasticsearchClient::new(base_url);
        let err = client
            .index("idx", "1", &serde_json::json!({}))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("status 400"), "got: {msg}");
        assert!(msg.contains("mapper parse error"), "got: {msg}");
    }

    #[tokio::test]
    async fn search_non_json_body_returns_parse_error() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let base_url = spawn_mock(captured.clone(), 200, "not json").await;
        let client = ElasticsearchClient::new(base_url);
        let err = client
            .search("idx", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("es parse"), "got: {err}");
    }
}

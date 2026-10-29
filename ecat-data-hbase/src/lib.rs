// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache HBase REST (Stargate) client.
//!
//! Wire format is the JSON CellSet documented in the HBase reference guide
//! ("Using REST Endpoints"): row keys, column names and values are Base-64
//! encoded; `PUT`/`GET` row endpoints carry the row key as a URL path segment
//! and a CellSet in the body. Every request asks for `application/json`.
//!
//! # Value convention
//!
//! Writes: [`serde_json::Value::String`] is stored as its raw UTF-8 bytes,
//! any other value as its `serde_json` serialization.
//!
//! Reads: a value that is valid UTF-8 and parses as JSON is returned as that
//! JSON value, other valid UTF-8 as a string, and invalid UTF-8 as a Base-64
//! string. The JSON-first rule is lossy for strings that look like JSON
//! (`"42"` written as a string reads back as the number `42`).
//!
//! Row keys follow the same rule: non-UTF-8 keys come back as Base-64.
//!
//! # Authentication
//!
//! Optional HTTP Basic via [`HbaseConfig::username`] / [`HbaseConfig::password`].
//! Kerberos / SPNEGO is **not supported**.
//!
//! # Scanning
//!
//! [`WideColumnClient::scan`] creates a stateful scanner
//! (`PUT /{table}/scanner`), pages it with `GET <Location>` until `limit` rows
//! or HTTP 204, then always attempts `DELETE <Location>` — including when
//! parsing fails. If the outer timeout fires first the future is dropped and
//! the scanner is left to the server's scanner timeout.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, WideColumnClient, breaker_error_to_backend_error, run_with_timeout};
use ecat_errors::{Error, ErrorCode};
use ecat_tls::TlsClientConfig;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

mod cellset;

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

use cellset::{Row, encode_value, parse_cellset, prefix_end};

#[derive(Debug, Clone, Deserialize)]
pub struct HbaseConfig {
    /// REST gateway base URL, e.g. `http://hbase-rest:8080`.
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
    /// 熔断**默认开启**；`BreakerConfig` 没有 `enabled` 开关，真要停用只能把
    /// 阈值调到不可能触发（如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = **不限并发**；未配置 = 32。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct HbaseClient {
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

pub(crate) fn backend(message: String) -> Error {
    Error::new(ErrorCode::Unavailable, "hbase", message)
}

fn reqwest_err(e: reqwest::Error) -> Error {
    backend(format!("hbase: {e}"))
}

impl HbaseClient {
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

    pub fn from_config(cfg: HbaseConfig) -> Result<Self, Error> {
        reqwest::Url::parse(&cfg.base_url).map_err(|e| {
            Error::new(
                ErrorCode::InvalidArgument,
                "hbase",
                format!("hbase: invalid base_url: {e}"),
            )
        })?;
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "hbase", format!("hbase tls: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线：`metrics` feature 下构造即注册（与其余后端一致，注册是幂等的）。
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
                // `0` = 不限并发：不建信号量。`Semaphore::new(0)` 会让每次调用
                // 静默无限挂起 —— `guarded` 的第一句就是 `permit().await`。
                Some(0) => None,
                Some(n) => Some(Arc::new(Semaphore::new(n))),
                None => Some(Arc::new(Semaphore::new(32))),
            },
        })
    }

    /// 本 client 的熔断器（`metrics` 注册与测试读它的状态）。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 取一个并发许可；不限并发时返回 `None`。信号量从不 `close()`。
    async fn permit(&self) -> Option<SemaphorePermit<'_>> {
        match &self.semaphore {
            Some(sem) => Some(sem.acquire().await.expect("semaphore is never closed")),
            None => None,
        }
    }

    /// 一次出站调用的公共外壳：**许可 → 熔断 → 超时**。
    ///
    /// 顺序不能改：超时若在外层，熔断器对卡死的后端永久失明（超时 drop 内层
    /// future，熔断器记失败的那句在 `f().await` 之后，窗口里什么都不记）。
    /// 许可在最外：还在排队的请求还没碰后端，不该计入熔断失败、也不该被超时掐断。
    ///
    /// 维度用 [`BackendKind::Document`]：宽列没有独立维度，按
    /// `ecat-data/src/timeout.rs` 的「跟 trait 家族走」判据，`WideColumnClient`
    /// 最接近既有的 `Document` 维度（同为 `ecat_errors::Error` 路径）。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Document, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "hbase"))
    }

    /// JSON 头 + 可选 Basic 认证。HBase REST 默认回纯文本，GET 整行必须带
    /// 能承载多列的 `Accept`（json）。
    fn json_request(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        ecat_tls::apply_basic_auth(
            req.header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::CONTENT_TYPE, "application/json"),
            &self.username,
            &self.password,
        )
    }

    /// `{base}/{seg1}/{seg2}/...`，逐段 percent-encode（row key 里的空格、`?` 等）。
    fn url(&self, segments: &[&str]) -> Result<reqwest::Url, Error> {
        let mut url = reqwest::Url::parse(&format!("{}/", self.base_url.trim_end_matches('/')))
            .map_err(|e| backend(format!("hbase: invalid base_url: {e}")))?;
        url.path_segments_mut()
            .map_err(|_| backend("hbase: base_url cannot be a base".into()))?
            .extend(segments);
        Ok(url)
    }

    /// Scanner 的 `Location` 可能是绝对 URL 或绝对路径。
    fn resolve_location(&self, location: &str) -> Result<reqwest::Url, Error> {
        reqwest::Url::parse(&self.base_url)
            .and_then(|base| base.join(location))
            .map_err(|e| backend(format!("hbase: bad scanner Location {location:?}: {e}")))
    }

    async fn response_error(resp: reqwest::Response) -> Error {
        let status = resp.status();
        let body = resp.bytes().await.unwrap_or_default();
        http_error(status, &body)
    }

    /// 创建 scanner 并翻页，最后**无条件**尝试关闭。
    async fn run_scanner(
        &self,
        create: reqwest::RequestBuilder,
        limit: usize,
    ) -> Result<Vec<Row>, Error> {
        let resp = create.send().await.map_err(reqwest_err)?;
        let status = resp.status();
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = resp
            .bytes()
            .await
            .map_err(|e| backend(format!("hbase: read scanner body: {e}")))?;
        if !status.is_success() {
            return Err(http_error(status, &body));
        }
        let location = location
            .ok_or_else(|| backend("hbase: scanner response has no Location header".into()))?;
        let scanner_url = self.resolve_location(&location)?;
        let result = self.collect_scanner(&scanner_url, &body, limit).await;
        let _ = self.close_scanner(&scanner_url).await;
        result
    }

    async fn collect_scanner(
        &self,
        scanner_url: &reqwest::Url,
        first_body: &[u8],
        limit: usize,
    ) -> Result<Vec<Row>, Error> {
        let mut rows = if first_body.is_empty() {
            Vec::new()
        } else {
            parse_cellset(first_body)?
        };
        while rows.len() < limit {
            let req = self.json_request(self.client.get(scanner_url.clone()));
            let resp = req.send().await.map_err(reqwest_err)?;
            if resp.status() == StatusCode::NO_CONTENT {
                break;
            }
            if !resp.status().is_success() {
                return Err(Self::response_error(resp).await);
            }
            let page = resp
                .bytes()
                .await
                .map_err(|e| backend(format!("hbase: read scanner page: {e}")))?;
            // 空 200 也当作扫描结束，否则会无限翻页。
            if page.is_empty() {
                break;
            }
            let page = parse_cellset(&page)?;
            if page.is_empty() {
                break;
            }
            rows.extend(page);
        }
        rows.truncate(limit);
        Ok(rows)
    }

    async fn close_scanner(&self, scanner_url: &reqwest::Url) -> Result<(), Error> {
        let req = self.json_request(self.client.delete(scanner_url.clone()));
        let resp = req.send().await.map_err(reqwest_err)?;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::response_error(resp).await)
        }
    }
}

fn validate_component(name: &'static str, value: &str) -> Result<(), Error> {
    // 拒绝路径分隔符与恰好为 `.`/`..` 的段；`a..b` 这类合法 row key 必须放行。
    if value.is_empty() || value.contains('/') || value == "." || value == ".." {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "hbase",
            format!("hbase: invalid {name} {value:?}"),
        ));
    }
    Ok(())
}

fn is_column_name(column: &str) -> bool {
    matches!(
        column.split_once(':'),
        Some((family, qualifier)) if !family.is_empty() && !qualifier.is_empty()
    )
}

fn http_error(status: StatusCode, body: &[u8]) -> Error {
    let code = match status.as_u16() {
        400 | 422 => ErrorCode::InvalidArgument,
        401 => ErrorCode::Unauthenticated,
        403 => ErrorCode::PermissionDenied,
        404 => ErrorCode::NotFound,
        409 => ErrorCode::AlreadyExists,
        429 => ErrorCode::ResourceExhausted,
        500..=599 => ErrorCode::Unavailable,
        _ => ErrorCode::Internal,
    };
    Error::new(
        code,
        "hbase",
        format!("hbase: HTTP {status}: {}", String::from_utf8_lossy(body)),
    )
}

#[async_trait]
impl WideColumnClient for HbaseClient {
    async fn put(
        &self,
        table: &str,
        row: &str,
        cells: &BTreeMap<String, Value>,
    ) -> Result<(), Error> {
        validate_component("table", table)?;
        validate_component("row", row)?;
        let mut encoded = Vec::with_capacity(cells.len());
        for (column, value) in cells {
            if !is_column_name(column) {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "hbase",
                    format!("hbase: invalid cell column {column:?} (expected family:qualifier)"),
                ));
            }
            encoded.push(json!({
                "column": B64.encode(column.as_bytes()),
                "$": B64.encode(encode_value(value)),
            }));
        }
        let cell_set = json!({
            "Row": [{ "key": B64.encode(row.as_bytes()), "Cell": encoded }],
        });
        let body = serde_json::to_vec(&cell_set)
            .map_err(|e| backend(format!("hbase: encode CellSet: {e}")))?;
        let req = self.json_request(self.client.put(self.url(&[table, row])?).body(body));
        self.guarded(async {
            let resp = req.send().await.map_err(reqwest_err)?;
            if !resp.status().is_success() {
                return Err(Self::response_error(resp).await);
            }
            Ok(())
        })
        .await
    }

    async fn get(&self, table: &str, row: &str) -> Result<Option<BTreeMap<String, Value>>, Error> {
        validate_component("table", table)?;
        validate_component("row", row)?;
        let req = self.json_request(self.client.get(self.url(&[table, row])?));
        self.guarded(async {
            let resp = req.send().await.map_err(reqwest_err)?;
            if resp.status() == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !resp.status().is_success() {
                return Err(Self::response_error(resp).await);
            }
            let body = resp
                .bytes()
                .await
                .map_err(|e| backend(format!("hbase: read row body: {e}")))?;
            let mut rows = parse_cellset(&body)?;
            Ok(rows.drain(..).next().map(|(_, cells)| cells))
        })
        .await
    }

    async fn scan(&self, table: &str, prefix: &str, limit: usize) -> Result<Vec<Row>, Error> {
        validate_component("table", table)?;
        // `limit == 0` = 用默认 1000（与配置里 `0` = 禁用的语义不同，这里 0
        // 只是「没指定」；返回 0 行没有意义）。
        let limit = if limit == 0 { 1000 } else { limit };
        let mut scanner = serde_json::Map::new();
        scanner.insert("startRow".into(), json!(B64.encode(prefix.as_bytes())));
        if let Some(end) = prefix_end(prefix.as_bytes()) {
            scanner.insert("endRow".into(), json!(B64.encode(end)));
        }
        scanner.insert("batch".into(), json!(limit.min(i32::MAX as usize)));
        let body = serde_json::to_vec(&Value::Object(scanner))
            .map_err(|e| backend(format!("hbase: encode scanner: {e}")))?;
        let req = self.json_request(self.client.put(self.url(&[table, "scanner"])?).body(body));
        self.guarded(async { self.run_scanner(req, limit).await })
            .await
    }

    /// 幂等：行不存在（404）也算成功。
    async fn delete(&self, table: &str, row: &str) -> Result<(), Error> {
        validate_component("table", table)?;
        validate_component("row", row)?;
        let req = self.json_request(self.client.delete(self.url(&[table, row])?));
        self.guarded(async {
            let resp = req.send().await.map_err(reqwest_err)?;
            if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
                return Ok(());
            }
            Err(Self::response_error(resp).await)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    mod protocol;
    mod resilience;
    use super::*;
}

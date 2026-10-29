// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Iceberg REST catalog client.
//!
//! Scope: **catalog metadata only** — namespaces, table identifiers, and raw
//! table metadata JSON. This crate does **not** read or decode Iceberg data
//! files (Parquet, Avro, ORC), manifests, or snapshots; planning and data
//! access belong to an execution engine.
//!
//! Paths follow the Iceberg REST OpenAPI spec under `/v1`. Multi-level
//! namespaces are joined with the unit separator (`\u{1F}`) and percent-encoded,
//! so `["accounting", "tax"]` becomes `accounting%1Ftax`. A catalog that
//! requires a non-empty `prefix` override from `/v1/config` is not rewired
//! automatically; include that prefix in `base_url`.

use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, breaker_error_to_backend_error, run_with_timeout};
use ecat_errors::{Error, ErrorCode};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

/// `Error::reason` 的组件标识。
const REASON: &str = "iceberg";
/// 分页上限。避免服务端始终返回同一个 token 时无限循环（每次请求本身仍受超时约束）。
const MAX_PAGES: usize = 1000;

#[derive(Debug, Clone, Deserialize)]
pub struct IcebergConfig {
    pub base_url: String,
    /// Bearer token，优先于 Basic 认证。
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = 不限并发；未配置 = 32。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct IcebergClient {
    client: reqwest::Client,
    base_url: String,
    bearer_token: Option<String>,
    username: Option<String>,
    password: Option<String>,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器挂在后端实例上，不是进程上。
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

impl IcebergClient {
    /// 无认证（本地 / 匿名 catalog）的 client，不做任何 I/O。
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            bearer_token: None,
            username: None,
            password: None,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    /// 同步构造（仅建 TLS client），不做任何 I/O。
    pub fn from_config(cfg: IcebergConfig) -> Result<Self, Error> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, REASON, format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        Ok(Self {
            client,
            base_url: cfg.base_url,
            bearer_token: cfg.bearer_token,
            username: cfg.username,
            password: cfg.password,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
            semaphore: match cfg.max_concurrency {
                // `0` = 不限并发：建 `Semaphore::new(0)` 会让每次调用永久挂起。
                Some(0) => None,
                Some(n) => Some(Arc::new(Semaphore::new(n))),
                None => Some(Arc::new(Semaphore::new(32))),
            },
        })
    }

    /// 本 client 的熔断器。
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
    /// 顺序不能改：超时若在外层，熔断器会对卡死的后端永久失明（见 questdb 的同名注释）。
    /// `BackendKind` 写死为 `Storage` —— 本 crate 不属于 `ecat-data` 的任何 trait
    /// 家族，Storage 是语义上最接近的维度；本 crate 不注册指标。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Storage, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, REASON))
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.bearer_token {
            Some(token) => req.bearer_auth(token),
            None => ecat_tls::apply_basic_auth(req, &self.username, &self.password),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn get_json(&self, url: &str) -> Result<Value, Error> {
        let req = self.apply_auth(self.client.get(url));
        self.guarded(async { json_body(req.send().await.map_err(transport)?).await })
            .await
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, Error> {
        let req = self.apply_auth(self.client.post(url).json(body));
        self.guarded(async { json_body(req.send().await.map_err(transport)?).await })
            .await
    }

    /// POST 但只关心成功与否（如 create namespace，响应体不建模）。
    async fn post_ok(&self, url: &str, body: &Value) -> Result<(), Error> {
        let req = self.apply_auth(self.client.post(url).json(body));
        self.guarded(async {
            checked(req.send().await.map_err(transport)?)
                .await
                .map(|_| ())
        })
        .await
    }

    async fn delete(&self, url: &str) -> Result<(), Error> {
        let req = self.apply_auth(self.client.delete(url));
        self.guarded(async {
            checked(req.send().await.map_err(transport)?)
                .await
                .map(|_| ())
        })
        .await
    }

    /// 跟随 `next-page-token` 收集某个列表端点所有页的条目。
    async fn list_pages(&self, path: &str, key: &str) -> Result<Vec<Value>, Error> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = self.url(path);
            if let Some(t) = &token {
                url.push_str("?pageToken=");
                url.push_str(&percent_encode_segment(t));
            }
            let page = self.get_json(&url).await?;
            let items = page.get(key).and_then(Value::as_array).ok_or_else(|| {
                Error::new(
                    ErrorCode::Internal,
                    REASON,
                    format!("response is missing `{key}`: {page}"),
                )
            })?;
            out.extend(items.iter().cloned());
            token = page
                .get("next-page-token")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            if token.is_none() {
                return Ok(out);
            }
        }
        Err(Error::new(
            ErrorCode::Internal,
            REASON,
            format!("pagination exceeded {MAX_PAGES} pages"),
        ))
    }

    /// `GET /v1/config` — 原样返回 `defaults` / `overrides`。
    pub async fn config(&self) -> Result<Value, Error> {
        self.get_json(&self.url("/v1/config")).await
    }

    /// `GET /v1/namespaces`（跟随 `next-page-token`）。
    pub async fn list_namespaces(&self) -> Result<Vec<Vec<String>>, Error> {
        let items = self.list_pages("/v1/namespaces", "namespaces").await?;
        Ok(items
            .iter()
            .filter_map(|ns| {
                ns.as_array().map(|parts| {
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
            })
            .collect())
    }

    /// `POST /v1/namespaces`；`properties` 省略时发送空对象（规范默认值）。
    pub async fn create_namespace(
        &self,
        namespace: &[String],
        properties: Option<&Value>,
    ) -> Result<(), Error> {
        namespace_segment(namespace)?;
        let body = json!({
            "namespace": namespace,
            "properties": properties.cloned().unwrap_or_else(|| json!({})),
        });
        self.post_ok(&self.url("/v1/namespaces"), &body).await
    }

    /// `GET /v1/namespaces/{namespace}/tables`（跟随 `next-page-token`）。
    pub async fn list_tables(&self, namespace: &[String]) -> Result<Vec<String>, Error> {
        let path = format!("/v1/namespaces/{}/tables", namespace_segment(namespace)?);
        let items = self.list_pages(&path, "identifiers").await?;
        Ok(items
            .iter()
            .filter_map(|i| i.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect())
    }

    /// `GET /v1/namespaces/{namespace}/tables/{table}` — 原样返回
    /// `metadata-location` 与 `metadata`。
    pub async fn load_table(&self, namespace: &[String], table: &str) -> Result<Value, Error> {
        let path = format!(
            "/v1/namespaces/{}/tables/{}",
            namespace_segment(namespace)?,
            percent_encode_segment(table)
        );
        self.get_json(&self.url(&path)).await
    }

    /// `POST /v1/namespaces/{namespace}/tables`；schema 原样透传，不建模类型系统。
    pub async fn create_table(
        &self,
        namespace: &[String],
        name: &str,
        schema: &Value,
        location: Option<&str>,
        properties: Option<&Value>,
    ) -> Result<Value, Error> {
        let mut body = json!({"name": name, "schema": schema});
        if let Some(location) = location {
            body["location"] = json!(location);
        }
        if let Some(properties) = properties {
            body["properties"] = properties.clone();
        }
        let path = format!("/v1/namespaces/{}/tables", namespace_segment(namespace)?);
        self.post_json(&self.url(&path), &body).await
    }

    /// `DELETE /v1/namespaces/{namespace}/tables/{table}`。
    pub async fn drop_table(&self, namespace: &[String], table: &str) -> Result<(), Error> {
        let path = format!(
            "/v1/namespaces/{}/tables/{}",
            namespace_segment(namespace)?,
            percent_encode_segment(table)
        );
        self.delete(&self.url(&path)).await
    }
}

fn transport(e: reqwest::Error) -> Error {
    Error::new(ErrorCode::Internal, REASON, format!("request: {e}"))
}

/// 2xx 原样返回；非 2xx 把服务端响应体作为错误消息（不猜测错误结构）。
async fn checked(resp: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(Error::new(
        ErrorCode::Internal,
        REASON,
        if body.is_empty() {
            format!("HTTP {status}")
        } else {
            body
        },
    ))
}

async fn json_body(resp: reqwest::Response) -> Result<Value, Error> {
    checked(resp)
        .await?
        .json()
        .await
        .map_err(|e| Error::new(ErrorCode::Internal, REASON, format!("parse: {e}")))
}

/// 多级命名空间在路径里用 unit separator (`0x1F`) 连接后再整体百分号编码，
/// 于是分隔符变成 `%1F`（Iceberg REST 规范）。
fn namespace_segment(namespace: &[String]) -> Result<String, Error> {
    if namespace.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            REASON,
            "namespace must contain at least one level",
        ));
    }
    Ok(percent_encode_segment(&namespace.join("\u{1f}")))
}

/// RFC 3986 段编码：仅保留 unreserved 字符（`A-Z a-z 0-9 - _ . ~`），
/// 其余字节写成 `%XX`（大写十六进制）。
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

#[cfg(test)]
mod tests;

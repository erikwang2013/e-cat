// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Delta Sharing read-only protocol client.
//!
//! Scope: **discovery and metadata only** — shares, schemas, tables, table
//! metadata/version, and the file list returned by the query endpoint. This
//! crate does **not** download or decode the Parquet files referenced by that
//! list; it returns their URLs and metadata for an execution engine.
//!
//! Protocol: <https://github.com/delta-io/delta-sharing/blob/main/PROTOCOL.md>.
//! `query_table` supports both the current NDJSON stream (`application/x-ndjson` /
//! `application/json-seq`) and the legacy `{ "files": [...], "nextPageToken": ... }`
//! envelope, chosen by `Content-Type`; listing and query results follow
//! `nextPageToken` and aggregate across pages.

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
const REASON: &str = "delta-sharing";
/// 分页上限。避免服务端始终返回同一个 token 时无限循环。
const MAX_PAGES: usize = 1000;

#[derive(Debug, Clone, Deserialize)]
pub struct DeltaSharingConfig {
    pub base_url: String,
    /// Delta Sharing 只支持 bearer token（必填）。
    pub bearer_token: String,
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

pub struct DeltaSharingClient {
    client: reqwest::Client,
    base_url: String,
    bearer_token: String,
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

impl DeltaSharingClient {
    /// 同步构造，不做任何 I/O。
    pub fn new(base_url: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            bearer_token: bearer_token.into(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    /// 同步构造（仅建 TLS client），不做任何 I/O。
    pub fn from_config(cfg: DeltaSharingConfig) -> Result<Self, Error> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, REASON, format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        Ok(Self {
            client,
            base_url: cfg.base_url,
            bearer_token: cfg.bearer_token,
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
    /// 顺序不能改（见 questdb 同名注释）；`BackendKind::Storage` 是最近似的维度。
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

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn get_json(&self, url: &str) -> Result<Value, Error> {
        let req = self.client.get(url).bearer_auth(&self.bearer_token);
        self.guarded(async { json_body(req.send().await.map_err(transport)?).await })
            .await
    }

    /// POST query body（带 `includeEndStreamAction` 能力头），返回 `(is_ndjson, 文本)`。
    /// `Content-Type` 必须在消费响应前读出来。
    async fn post_query(&self, url: &str, body: &Value) -> Result<(bool, String), Error> {
        let req = self
            .client
            .post(url)
            .bearer_auth(&self.bearer_token)
            .header("delta-sharing-capabilities", "includeEndStreamAction=true")
            .json(body);
        self.guarded(async {
            let resp = req.send().await.map_err(transport)?;
            let ndjson = is_ndjson(&resp);
            let resp = checked(resp).await?;
            let text = resp.text().await.map_err(transport)?;
            Ok((ndjson, text))
        })
        .await
    }

    /// 跟随 `nextPageToken` 收集列表端点的 `items[].name`。
    async fn list_names(&self, path: &str) -> Result<Vec<String>, Error> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = self.url(path);
            if let Some(t) = &token {
                url.push_str("?pageToken=");
                url.push_str(&percent_encode_segment(t));
            }
            let page = self.get_json(&url).await?;
            let items = page
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| parse_error(format!("response is missing `items`: {page}")))?;
            out.extend(
                items
                    .iter()
                    .filter_map(|i| i.get("name").and_then(Value::as_str).map(str::to_owned)),
            );
            token = next_token(&page);
            if token.is_none() {
                return Ok(out);
            }
        }
        Err(too_many_pages())
    }

    /// `GET /shares`（自动分页）。
    pub async fn list_shares(&self) -> Result<Vec<String>, Error> {
        self.list_names("/shares").await
    }

    /// `GET /shares/{share}/schemas`（自动分页）。
    pub async fn list_schemas(&self, share: &str) -> Result<Vec<String>, Error> {
        let share = percent_encode_segment(share);
        self.list_names(&format!("/shares/{share}/schemas")).await
    }

    /// `GET /shares/{share}/schemas/{schema}/tables`（自动分页）。
    pub async fn list_tables(&self, share: &str, schema: &str) -> Result<Vec<String>, Error> {
        let share = percent_encode_segment(share);
        let schema = percent_encode_segment(schema);
        self.list_names(&format!("/shares/{share}/schemas/{schema}/tables"))
            .await
    }

    /// `GET .../tables/{table}/metadata` — 原样返回 JSON。
    pub async fn table_metadata(
        &self,
        share: &str,
        schema: &str,
        table: &str,
    ) -> Result<Value, Error> {
        self.get_json(&self.url(&table_path(share, schema, table, "metadata")))
            .await
    }

    /// `GET .../tables/{table}/version` — 原样返回 JSON。
    pub async fn table_version(
        &self,
        share: &str,
        schema: &str,
        table: &str,
    ) -> Result<Value, Error> {
        self.get_json(&self.url(&table_path(share, schema, table, "version")))
            .await
    }

    /// `POST .../tables/{table}/query` — 请求体原样透传（`{}` 表示全部文件），
    /// 并带 `delta-sharing-capabilities: includeEndStreamAction=true`。
    ///
    /// 响应按 `Content-Type` 分流：`application/x-ndjson` / `application/json-seq`
    /// 逐行解析 action（wrapper 和裸 EndStreamAction 都接受），其他类型按旧的
    /// `{files,nextPageToken}` JSON 信封解析。
    ///
    /// 返回文件的 **URL / 元数据列表，不下载也不解码文件内容**。分页时逐页
    /// POST，把 `nextPageToken`（JSON 顶层或 `endStreamAction` 内）放回请求体
    /// 的 `pageToken`。`files` 聚合所有页，`protocol`/`metadata` 取首次出现的值，
    /// `refreshToken`/`minUrlExpirationTimestamp` 取最后一次非空值（用于刷新
    /// 预签名 URL），`nextPageToken` 被移除。
    pub async fn query_table(
        &self,
        share: &str,
        schema: &str,
        table: &str,
        request: &Value,
    ) -> Result<Value, Error> {
        if !request.is_object() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                REASON,
                "query request must be a JSON object",
            ));
        }
        let url = self.url(&table_path(share, schema, table, "query"));
        let mut body = request.clone();
        let mut files = Vec::new();
        let mut protocol: Option<Value> = None;
        let mut metadata: Option<Value> = None;
        let mut envelope: Option<Value> = None;
        let mut refresh_token: Option<Value> = None;
        let mut min_url_expiration: Option<Value> = None;

        for _ in 0..MAX_PAGES {
            let (ndjson, text) = self.post_query(&url, &body).await?;
            let page = parse_query_page(ndjson, &text)?;
            files.extend(page.files);
            if protocol.is_none() {
                protocol = page.protocol;
            }
            if metadata.is_none() {
                metadata = page.metadata;
            }
            if envelope.is_none() {
                envelope = page.envelope;
            }
            refresh_token = page.refresh_token.or(refresh_token);
            min_url_expiration = page.min_url_expiration.or(min_url_expiration);
            match page.next {
                Some(t) => body["pageToken"] = json!(t),
                None => {
                    let mut out = envelope.unwrap_or_else(|| json!({}));
                    let obj = out
                        .as_object_mut()
                        .ok_or_else(|| parse_error("query response is not a JSON object"))?;
                    obj.insert("files".into(), Value::Array(files));
                    for (k, v) in [
                        ("protocol", protocol),
                        ("metadata", metadata),
                        ("refreshToken", refresh_token),
                        ("minUrlExpirationTimestamp", min_url_expiration),
                    ] {
                        if let Some(v) = v {
                            obj.insert(k.into(), v);
                        }
                    }
                    obj.remove("nextPageToken");
                    return Ok(out);
                }
            }
        }
        Err(too_many_pages())
    }
}

fn table_path(share: &str, schema: &str, table: &str, suffix: &str) -> String {
    let share = percent_encode_segment(share);
    let schema = percent_encode_segment(schema);
    let table = percent_encode_segment(table);
    format!("/shares/{share}/schemas/{schema}/tables/{table}/{suffix}")
}

fn next_token(page: &Value) -> Option<String> {
    page.get("nextPageToken")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

fn parse_error(message: impl Into<String>) -> Error {
    let message = message.into();
    Error::new(ErrorCode::Internal, REASON, format!("parse: {message}"))
}

/// 当前协议用 `application/x-ndjson`；`application/json-seq`（RFC 7464）同样逐行。
fn is_ndjson(resp: &reqwest::Response) -> bool {
    let ct = resp.headers().get(reqwest::header::CONTENT_TYPE);
    let ct = ct.and_then(|v| v.to_str().ok()).unwrap_or_default();
    let ct = ct.split(';').next().unwrap_or_default().trim();
    ct.eq_ignore_ascii_case("application/x-ndjson")
        || ct.eq_ignore_ascii_case("application/json-seq")
}

/// 一页 query 响应，统一承载两种协议。
struct QueryPage {
    /// 旧 JSON 信封的原始对象（只有第一页用于拼装返回值）。
    envelope: Option<Value>,
    protocol: Option<Value>,
    metadata: Option<Value>,
    files: Vec<Value>,
    next: Option<String>,
    refresh_token: Option<Value>,
    min_url_expiration: Option<Value>,
}

fn parse_query_page(ndjson: bool, text: &str) -> Result<QueryPage, Error> {
    if !ndjson {
        let page: Value =
            serde_json::from_str(text).map_err(|e| parse_error(format!("invalid JSON: {e}")))?;
        let obj = page
            .as_object()
            .ok_or_else(|| parse_error("query response is not a JSON object"))?;
        return Ok(QueryPage {
            protocol: obj.get("protocol").cloned(),
            // 旧信封用 `metadata`；同样接受规范里的 `metaData`。
            metadata: obj.get("metadata").or_else(|| obj.get("metaData")).cloned(),
            files: obj
                .get("files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            next: next_token(&page),
            envelope: Some(page),
            refresh_token: None,
            min_url_expiration: None,
        });
    }

    let mut page = QueryPage {
        envelope: None,
        protocol: None,
        metadata: None,
        files: Vec::new(),
        next: None,
        refresh_token: None,
        min_url_expiration: None,
    };
    for line in text.lines() {
        // RFC 7464 每条记录以 RS (0x1E) 开头；空行（含末尾换行）忽略。
        let line = line.trim().trim_start_matches('\u{1e}').trim();
        if line.is_empty() {
            continue;
        }
        let action: Value = serde_json::from_str(line)
            .map_err(|e| parse_error(format!("invalid NDJSON line: {e}")))?;
        let obj = action
            .as_object()
            .ok_or_else(|| parse_error("NDJSON line is not a JSON object"))?;
        let mut actions = 0;
        if let Some(v) = obj.get("protocol") {
            actions += 1;
            page.protocol.get_or_insert_with(|| v.clone());
        }
        if let Some(v) = obj.get("metaData").or_else(|| obj.get("metadata")) {
            actions += 1;
            page.metadata.get_or_insert_with(|| v.clone());
        }
        if let Some(v) = obj.get("file") {
            actions += 1;
            page.files.push(v.clone());
        }
        // PROTOCOL.md 的示例是裸对象，Scala 参考客户端用 `{"endStreamAction":…}`：
        // 无 wrapper、不含其他 action、且带 EndStreamAction 字段的行按裸形状处理。
        let bare_end = actions == 0
            && (obj.contains_key("refreshToken")
                || obj.contains_key("minUrlExpirationTimestamp")
                || obj.contains_key("nextPageToken")
                || obj.contains_key("errorMessage"));
        if let Some(v) = obj.get("endStreamAction").or(bare_end.then_some(&action)) {
            actions += 1;
            // 规范要求客户端在 `errorMessage` 非空时让整次查询失败。
            if let Some(msg) = v
                .get("errorMessage")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
            {
                return Err(Error::new(
                    ErrorCode::Internal,
                    REASON,
                    format!("query stream error: {msg}"),
                ));
            }
            page.next = next_token(v);
            page.refresh_token = v.get("refreshToken").cloned().or(page.refresh_token);
            page.min_url_expiration = v
                .get("minUrlExpirationTimestamp")
                .cloned()
                .or(page.min_url_expiration);
        }
        match actions {
            0 => {
                let key = obj.keys().next().map(String::as_str).unwrap_or("<empty>");
                return Err(parse_error(format!("unknown NDJSON action `{key}`")));
            }
            1 => {}
            _ => return Err(parse_error("NDJSON line contains more than one action")),
        }
    }
    Ok(page)
}

fn too_many_pages() -> Error {
    Error::new(
        ErrorCode::Internal,
        REASON,
        format!("pagination exceeded {MAX_PAGES} pages"),
    )
}

fn transport(e: reqwest::Error) -> Error {
    Error::new(ErrorCode::Internal, REASON, format!("request: {e}"))
}

/// 2xx 原样返回；非 2xx 把服务端响应体作为错误消息。
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

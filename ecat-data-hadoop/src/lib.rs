// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Hadoop WebHDFS storage client.
//!
//! HDFS has no bucket concept: the [`StorageClient`] `bucket` maps to a
//! first-level directory, i.e. `key` lives at `/{bucket}/{key}`. Both parts are
//! validated (no empty/absolute/dot segments, no backslashes) and every segment
//! is percent-encoded before it reaches the URL.
//!
//! Authentication is WebHDFS `user.name` query-parameter auth, optionally with
//! a `delegation` token. Kerberos / SPNEGO is **not** supported: it needs a
//! GSSAPI handshake that a plain reqwest client cannot perform.
//!
//! `CREATE` follows the documented two-step flow: the first `PUT` carries **no
//! body** and answers 307 + `Location`; the body then goes to that datanode
//! URL in a second `PUT`. Automatic redirects are therefore disabled on the
//! HTTP client (a followed 307 would replay the body to the wrong hop).

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, StorageClient, breaker_error_to_backend_error, run_with_timeout};
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
mod path;
use path::{
    encode_key_path, percent_encode_segment, valid_child_name, validate_bucket, validate_key,
    validate_prefix,
};

/// Upper bound on `FileStatus` entries visited by one `list` call (all
/// recursively walked directories), so a runaway tree fails loudly instead of
/// recursing forever.
const MAX_LIST_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Deserialize)]
pub struct HadoopConfig {
    pub base_url: String,
    /// WebHDFS `user.name`；缺省 `"ecat"`。
    #[serde(default = "default_user_name")]
    pub user_name: String,
    /// WebHDFS `delegation` token（安全模式集群）。Kerberos/SPNEGO 不支持。
    #[serde(default)]
    pub delegation_token: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次调用超时秒数（含 CREATE/OPEN 的两跳）。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（本 crate 两个构造器建的
    /// client 都有 30 秒）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    ///
    /// 熔断**默认开启**，没有 `enabled` 总开关；真要停用只能把阈值调到
    /// 不可能触发（如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = **不限并发**；未配置 = 32。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

fn default_user_name() -> String {
    "ecat".into()
}

pub struct HadoopClient {
    client: reqwest::Client,
    base_url: String,
    user_name: String,
    delegation_token: Option<String>,
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

/// Build the no-redirect client every constructor uses (see the CREATE note
/// in the crate docs).
fn no_redirect_client(tls: &Option<TlsClientConfig>) -> Result<reqwest::Client, String> {
    ecat_tls::build_reqwest_client_builder(tls)?
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("build client: {e}"))
}

fn io_error(op: &str, e: reqwest::Error) -> Error {
    Error::new(ErrorCode::Internal, "hadoop", format!("hadoop {op}: {e}"))
}

fn http_error(op: &str, status: reqwest::StatusCode, body: String) -> Error {
    let code = match status {
        reqwest::StatusCode::NOT_FOUND => ErrorCode::NotFound,
        reqwest::StatusCode::UNAUTHORIZED => ErrorCode::Unauthenticated,
        reqwest::StatusCode::FORBIDDEN => ErrorCode::PermissionDenied,
        _ => ErrorCode::Internal,
    };
    Error::new(
        code,
        "hadoop",
        format!("hadoop {op}: HTTP {status}: {body}"),
    )
}

fn location_header(resp: &reqwest::Response) -> Result<String, Error> {
    resp.headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Internal,
                "hadoop",
                "hadoop: redirect response without Location",
            )
        })
}

/// `noredirect=true`-style responses carry the datanode URL in JSON.
fn json_location(body: &serde_json::Value) -> Result<String, Error> {
    body.get("Location")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Internal,
                "hadoop",
                "hadoop: response without Location",
            )
        })
}

#[derive(Deserialize)]
struct ListStatusResponse {
    #[serde(rename = "FileStatuses")]
    file_statuses: FileStatuses,
}

#[derive(Deserialize)]
struct FileStatuses {
    #[serde(rename = "FileStatus", default)]
    file_status: Vec<FileStatus>,
}

#[derive(Deserialize)]
struct FileStatus {
    #[serde(rename = "pathSuffix", default)]
    path_suffix: String,
    #[serde(rename = "type", default)]
    kind: String,
}

impl HadoopClient {
    /// Unencrypted client with the default `"ecat"`-style settings (30 s
    /// timeout, default breaker, 32 concurrent calls).
    pub fn new(base_url: impl Into<String>, user_name: impl Into<String>) -> Self {
        Self {
            client: no_redirect_client(&None).expect("default reqwest client must build"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            user_name: user_name.into(),
            delegation_token: None,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn from_config(cfg: HadoopConfig) -> Result<Self, Error> {
        let client = no_redirect_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "hadoop", format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线：`metrics` feature 下构造即注册（注册是幂等的覆盖）。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            user_name: cfg.user_name,
            delegation_token: cfg.delegation_token,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
            semaphore: match cfg.max_concurrency {
                // `0` = 不限并发；建 `Semaphore::new(0)` 会让每次调用静默挂起。
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

    /// 一次出站调用的公共外壳：**许可 → 熔断 → 超时**（顺序同其余后端）。
    /// `Storage` 维度必须写死：超时计数与指标的 kind 都靠它。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Storage, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "hadoop"))
    }

    fn url(&self, path: &str, params: &[(&str, &str)]) -> String {
        let mut url = format!("{}/webhdfs/v1{path}", self.base_url);
        let mut sep = '?';
        let auth = params
            .iter()
            .copied()
            .chain(std::iter::once(("user.name", self.user_name.as_str())));
        for (k, v) in auth {
            url.push(sep);
            sep = '&';
            url.push_str(k);
            url.push('=');
            url.push_str(&percent_encode_segment(v));
        }
        if let Some(token) = &self.delegation_token {
            url.push(sep);
            url.push_str("delegation=");
            url.push_str(&percent_encode_segment(token));
        }
        url
    }

    fn object_path(bucket: &str, key: &str) -> Result<String, Error> {
        validate_bucket(bucket)?;
        validate_key(key)?;
        Ok(format!(
            "/{}/{}",
            percent_encode_segment(bucket),
            encode_key_path(key)
        ))
    }
}

#[async_trait]
impl StorageClient for HadoopClient {
    async fn put(&self, bucket: &str, key: &str, data: &[u8]) -> Result<(), Error> {
        let path = Self::object_path(bucket, key)?;
        let url = self.url(&path, &[("op", "CREATE"), ("overwrite", "true")]);
        self.guarded(async {
            // 第一跳：不带 body。307 → Location；直接 201 也接受。
            let resp = self
                .client
                .put(url)
                .send()
                .await
                .map_err(|e| io_error("put", e))?;
            let status = resp.status();
            let location = if status == reqwest::StatusCode::TEMPORARY_REDIRECT
                || status == reqwest::StatusCode::PERMANENT_REDIRECT
            {
                location_header(&resp)?
            } else if status == reqwest::StatusCode::CREATED {
                return Ok(());
            } else if status.is_success() {
                let body: serde_json::Value = resp.json().await.map_err(|e| io_error("put", e))?;
                json_location(&body)?
            } else {
                return Err(http_error(
                    "put",
                    status,
                    resp.text().await.unwrap_or_default(),
                ));
            };

            // 第二跳：body 只发给 datanode。
            let resp = self
                .client
                .put(location)
                .body(data.to_vec())
                .send()
                .await
                .map_err(|e| io_error("put", e))?;
            if !resp.status().is_success() {
                let status = resp.status();
                return Err(http_error(
                    "put",
                    status,
                    resp.text().await.unwrap_or_default(),
                ));
            }
            Ok(())
        })
        .await
    }

    async fn get(&self, bucket: &str, key: &str) -> Result<Vec<u8>, Error> {
        let path = Self::object_path(bucket, key)?;
        let url = self.url(&path, &[("op", "OPEN")]);
        self.guarded(async {
            let resp = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|e| io_error("get", e))?;
            let status = resp.status();
            // OPEN 通常 307 到 datanode；我们也接受直接 200 的数据流。
            let resp = if status == reqwest::StatusCode::TEMPORARY_REDIRECT
                || status == reqwest::StatusCode::PERMANENT_REDIRECT
            {
                let location = location_header(&resp)?;
                self.client
                    .get(location)
                    .send()
                    .await
                    .map_err(|e| io_error("get", e))?
            } else {
                resp
            };
            if !resp.status().is_success() {
                let status = resp.status();
                return Err(http_error(
                    "get",
                    status,
                    resp.text().await.unwrap_or_default(),
                ));
            }
            Ok(resp
                .bytes()
                .await
                .map_err(|e| io_error("get body", e))?
                .to_vec())
        })
        .await
    }

    async fn delete(&self, bucket: &str, key: &str) -> Result<(), Error> {
        let path = Self::object_path(bucket, key)?;
        let url = self.url(&path, &[("op", "DELETE"), ("recursive", "false")]);
        self.guarded(async {
            let resp = self
                .client
                .delete(url)
                .send()
                .await
                .map_err(|e| io_error("delete", e))?;
            if !resp.status().is_success() {
                let status = resp.status();
                return Err(http_error(
                    "delete",
                    status,
                    resp.text().await.unwrap_or_default(),
                ));
            }
            let body: serde_json::Value = resp.json().await.map_err(|e| io_error("delete", e))?;
            if body.get("boolean").and_then(|v| v.as_bool()) == Some(true) {
                Ok(())
            } else {
                Err(Error::new(
                    ErrorCode::Internal,
                    "hadoop",
                    format!("hadoop delete: server did not delete {path}"),
                ))
            }
        })
        .await
    }

    /// 整个递归遍历在**一次** `guarded` 内：一次 `list` 的预算是整次调用，
    /// 不是每页一个。超过 [`MAX_LIST_ENTRIES`] 的条目直接报错。
    async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, Error> {
        validate_bucket(bucket)?;
        validate_prefix(prefix)?;
        let root = format!("/{}", percent_encode_segment(bucket));
        self.guarded(async {
            let mut queue = vec![String::new()];
            let mut keys = Vec::new();
            let mut seen = 0usize;
            while let Some(dir) = queue.pop() {
                let path = if dir.is_empty() {
                    root.clone()
                } else {
                    format!("{root}/{}", encode_key_path(&dir))
                };
                let url = self.url(&path, &[("op", "LISTSTATUS")]);
                let resp = self
                    .client
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| io_error("list", e))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    return Err(http_error(
                        "list",
                        status,
                        resp.text().await.unwrap_or_default(),
                    ));
                }
                let listing: ListStatusResponse =
                    resp.json().await.map_err(|e| io_error("list", e))?;
                for st in listing.file_statuses.file_status {
                    seen += 1;
                    if seen > MAX_LIST_ENTRIES {
                        return Err(Error::new(
                            ErrorCode::ResourceExhausted,
                            "hadoop",
                            format!(
                                "hadoop list: more than {MAX_LIST_ENTRIES} entries under {bucket}"
                            ),
                        ));
                    }
                    if !valid_child_name(&st.path_suffix) {
                        return Err(Error::new(
                            ErrorCode::Internal,
                            "hadoop",
                            format!("hadoop list: invalid pathSuffix {:?}", st.path_suffix),
                        ));
                    }
                    let child = if dir.is_empty() {
                        st.path_suffix
                    } else {
                        format!("{dir}/{}", st.path_suffix)
                    };
                    match st.kind.as_str() {
                        "DIRECTORY" => queue.push(child),
                        // 只返回文件；其它类型（如 SYMLINK）跳过。
                        "FILE" if child.starts_with(prefix) => keys.push(child),
                        _ => {}
                    }
                }
            }
            Ok(keys)
        })
        .await
    }
}

#[cfg(test)]
mod tests;

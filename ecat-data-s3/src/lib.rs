// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! S3 / MinIO object storage client (reqwest + rustls, AWS SigV4 signing).
//!
//! TLS is handled through the shared [`ecat_tls::TlsClientConfig`] surface
//! (custom CA, mTLS, skip_verify), consistent with the other HTTP data
//! crates. `endpoint` without a scheme defaults to `https://`; prefix an
//! explicit `http://` (e.g. local MinIO) to opt out. `tls.skip_verify`
//! covers the "insecure" case.
//!
//! Requests are signed with AWS Signature V4 (path-style addressing) and
//! every response status is checked — non-2xx responses surface the status
//! and body instead of being silently dropped.

mod signing;
mod xml;

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, StorageClient, breaker_error_to_backend_error, run_with_timeout};
use ecat_errors::{Error as StorageError, ErrorCode};
use ecat_tls::TlsClientConfig;
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use signing::{Credentials, SigTime, canonical_query, encode_uri_component, hex, sign};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::{Semaphore, SemaphorePermit};

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

#[derive(Debug, Clone, Deserialize)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒）取先到者（本 crate 只有 `from_config` 一个构造器）。
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

pub struct S3Client {
    client: reqwest::Client,
    endpoint: String,
    host: String,
    region: String,
    access_key: String,
    secret_key: String,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
}

impl S3Client {
    pub fn from_config(cfg: S3Config) -> Result<Self, StorageError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| StorageError::new(ErrorCode::Internal, "s3", format!("s3 tls: {e}")))?;
        // 无 scheme 的 endpoint 默认 https（凭据走加密链路）；显式
        // "http://" 前缀是唯一的明文 opt-out（本地 MinIO 开发）。
        let endpoint = if cfg.endpoint.contains("://") {
            cfg.endpoint.clone()
        } else {
            format!("https://{}", cfg.endpoint)
        };
        let host = endpoint
            .strip_prefix("https://")
            .or_else(|| endpoint.strip_prefix("http://"))
            .unwrap_or(&endpoint)
            .to_string();
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线（lead 裁决 2026-10-08）：`metrics` feature 下**构造即注册**，
        // 用户代码零变化。`ecat_metrics::register_outbound_metrics` 是幂等的
        // （同一 backend 重复注册是覆盖闭包），所以多 client 不会炸。
        // 探针：注释掉下面两行 ⇒ from_config_registers_outbound_metrics 红。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            endpoint,
            host,
            region: cfg.region,
            access_key: cfg.access_key,
            secret_key: cfg.secret_key,
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
    /// `kind` **写死**不收参数：本 crate 的四个 I/O 方法同属 `StorageClient` 一个家族
    /// （ClickHouse 收参数是因为它有 `SqlExecutor` / `TsdbClient` 两条**不同家族**的路径
    /// 共用外壳）。多一个永不变化的入参就多一个填错的机会，
    /// 而填错只是静默少数（`ecat-data/src/timeout.rs:15-35`），没有编译期保护。
    ///
    /// **壳的边界 = 公开方法**：`list` 的翻页循环每页各发一次 HTTP，所以**整个
    /// 翻页循环**在这一个壳内；`signed_request` / `object_path` / `check_status`
    /// 是纯本地 helper（不发 HTTP）**不许包** —— 包了会在半开态下把探测名额
    /// 以 `Ok` 白白消耗。判据：问「这一步发 HTTP 吗？」——
    /// `whole_call_budget_covers_every_page_in_list` 盯着前一条。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, StorageError>
    where
        F: std::future::Future<Output = Result<T, StorageError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Storage, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "s3"))
    }

    /// 返回原始（未编码）路径；编码统一在 signed_request 的 URL 构建与
    /// sign 的 canonical URI 处各做一次，避免双重 percent-encoding。
    fn object_path(&self, bucket: &str, key: &str) -> String {
        format!("/{bucket}/{key}")
    }

    fn signed_request(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        payload: &[u8],
    ) -> (String, String, String, String) {
        let now = OffsetDateTime::now_utc();
        let payload_hash = hex(&Sha256::digest(payload));
        let time = SigTime {
            amz_date: now.format(&signing::AMZ_DATE_FMT).expect("amz date format"),
            date_stamp: now
                .format(&signing::DATE_STAMP_FMT)
                .expect("date stamp format"),
        };
        let auth = sign(
            method,
            &self.host,
            path,
            query,
            &payload_hash,
            &Credentials {
                access_key: &self.access_key,
                secret_key: &self.secret_key,
                region: &self.region,
            },
            &time,
        );
        let q = canonical_query(query);
        let url = if q.is_empty() {
            format!("{}{}", self.endpoint, encode_uri_component(path, true))
        } else {
            format!("{}{}?{q}", self.endpoint, encode_uri_component(path, true))
        };
        (url, auth, time.amz_date, payload_hash)
    }

    async fn check_status(
        resp: reqwest::Response,
        op: &str,
    ) -> Result<reqwest::Response, StorageError> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(StorageError::new(
                ErrorCode::Internal,
                "s3",
                format!("s3 {op}: HTTP {status}: {body}"),
            ));
        }
        Ok(resp)
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
impl StorageClient for S3Client {
    async fn put(&self, bucket: &str, key: &str, data: &[u8]) -> Result<(), StorageError> {
        let path = self.object_path(bucket, key);
        let (url, auth, amz_date, payload_hash) = self.signed_request("PUT", &path, &[], data);
        self.guarded(async {
            let resp = self
                .client
                .put(url)
                .header(AUTHORIZATION, auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", payload_hash)
                .body(data.to_vec())
                .send()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 put: {e}"))
                })?;
            Self::check_status(resp, "put").await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError> {
        let path = self.object_path(bucket, key);
        let (url, auth, amz_date, payload_hash) = self.signed_request("GET", &path, &[], b"");
        self.guarded(async {
            let resp = self
                .client
                .get(url)
                .header(AUTHORIZATION, auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", payload_hash)
                .send()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 get: {e}"))
                })?;
            let resp = Self::check_status(resp, "get").await?;
            Ok(resp
                .bytes()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 get body: {e}"))
                })?
                .to_vec())
        })
        .await
    }

    async fn delete(&self, bucket: &str, key: &str) -> Result<(), StorageError> {
        let path = self.object_path(bucket, key);
        let (url, auth, amz_date, payload_hash) = self.signed_request("DELETE", &path, &[], b"");
        self.guarded(async {
            let resp = self
                .client
                .delete(url)
                .header(AUTHORIZATION, auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", payload_hash)
                .send()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 delete: {e}"))
                })?;
            Self::check_status(resp, "delete").await?;
            Ok(())
        })
        .await
    }

    /// List object keys under `prefix`, following continuation tokens across
    /// pages (same behavior as the previous rust-s3 backend).
    ///
    /// **整个翻页循环一个预算**：一次 `list` 可能发很多次 GET（continuation token），
    /// 预算必须罩住整次调用，否则一次调用会变成 N 个独立预算
    /// （`whole_call_budget_covers_every_page_in_list` 盯着这条）。
    async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, StorageError> {
        let path = format!("/{bucket}");
        self.guarded(async {
            let mut keys = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let mut query: Vec<(&str, &str)> = vec![("list-type", "2"), ("prefix", prefix)];
                // 每页的签名与 URL 都是纯本地构造，随循环留在**同一个**壳内。
                let token_owned;
                if let Some(t) = &token {
                    token_owned = t.clone();
                    query.push(("continuation-token", &token_owned));
                }
                let (url, auth, amz_date, payload_hash) =
                    self.signed_request("GET", &path, &query, b"");
                let resp = self
                    .client
                    .get(url)
                    .header(AUTHORIZATION, auth)
                    .header("x-amz-date", amz_date)
                    .header("x-amz-content-sha256", payload_hash)
                    .send()
                    .await
                    .map_err(|e| {
                        StorageError::new(ErrorCode::Internal, "s3", format!("s3 list: {e}"))
                    })?;
                let resp = Self::check_status(resp, "list").await?;
                let body = resp.text().await.map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 list body: {e}"))
                })?;
                let (page_keys, next) = xml::parse_list_xml(&body);
                keys.extend(page_keys);
                match next {
                    Some(t) => token = Some(t),
                    None => break,
                }
            }
            Ok(keys)
        })
        .await
    }
}

#[cfg(test)]
mod tests;

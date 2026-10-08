// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! InfluxDB 2.x client (line protocol writer + Flux query).
//!
//! Measurement、tag key/value、field key 按 line protocol 转义（`,`、` `、
//! `=`、`\`）；字符串 field 值只需转义 `"` 和 `\`（引号内的逗号与空格
//! 合法）。tag/field 输出按 key 排序，保证行协议确定性。

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, DataPoint, FieldValue, TsdbClient, breaker_error_to_backend_error,
    run_with_timeout,
};
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
pub struct InfluxConfig {
    pub base_url: String,
    pub org: String,
    pub bucket: String,
    pub token: String,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒、`new` 没有）取先到者。
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

pub struct InfluxClient {
    client: reqwest::Client,
    write_url: String,
    query_url: String,
    org: String,
    bucket: String,
    token: String,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
}

impl InfluxClient {
    pub fn new(
        base_url: impl Into<String>,
        org: impl Into<String>,
        bucket: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        let base = base_url.into();
        Self {
            write_url: format!("{base}/api/v2/write"),
            query_url: format!("{base}/api/v2/query"),
            org: org.into(),
            bucket: bucket.into(),
            token: token.into(),
            client: reqwest::Client::new(),
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn from_config(cfg: InfluxConfig) -> Result<Self, Error> {
        let base = cfg.base_url.clone();
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "influx", format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线（lead 裁决 2026-10-08）：`metrics` feature 下**构造即注册**，
        // 用户代码零变化。`ecat_metrics::register_outbound_metrics` 是幂等的
        // （同一 backend 重复注册是覆盖闭包），所以多 client 不会炸。
        // 探针：注释掉下面两行 ⇒ from_config_registers_outbound_metrics 红。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            write_url: format!("{base}/api/v2/write"),
            query_url: format!("{base}/api/v2/query"),
            org: cfg.org,
            bucket: cfg.bucket,
            token: cfg.token,
            client,
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
    /// `kind` **写死**不收参数：本 crate 的两个 I/O 方法同属 `TsdbClient` 一个家族
    /// （ClickHouse 收参数是因为它有 `SqlExecutor` / `TsdbClient` 两条**不同家族**的路径
    /// 共用外壳）。多一个永不变化的入参就多一个填错的机会，
    /// 而填错只是静默少数（`ecat-data/src/timeout.rs:15-35`），没有编译期保护。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "influxdb"))
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

/// Escape a measurement, tag key/value or field key per InfluxDB line
/// protocol: backslash, comma, space and `=` must be escaped in these
/// unquoted parts of a line.
fn escape_line_part(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | ',' | ' ' | '=' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Escape a string field value per InfluxDB line protocol: only backslash
/// and double quote are required inside the quoted value; comma and space
/// are legal verbatim there.
fn escape_field_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[async_trait]
impl TsdbClient for InfluxClient {
    async fn write(&self, points: &[DataPoint]) -> Result<(), Error> {
        let mut lines = String::new();
        for p in points {
            // 经 BTreeMap 排序后输出：tag/field 顺序确定，行协议可复现
            let tags: String = if p.tags.is_empty() {
                String::new()
            } else {
                p.tags
                    .iter()
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .iter()
                    .map(|(k, v)| format!(",{}={}", escape_line_part(k), escape_line_part(v)))
                    .collect::<Vec<_>>()
                    .join("")
            };
            let fields: String = p
                .fields
                .iter()
                .collect::<std::collections::BTreeMap<_, _>>()
                .iter()
                .map(|(k, v)| {
                    let k = escape_line_part(k);
                    match v {
                        FieldValue::Float(f) => format!("{k}={f}"),
                        FieldValue::Int(i) => format!("{k}={i}i"),
                        FieldValue::String(s) => format!("{k}=\"{}\"", escape_field_string(s)),
                        FieldValue::Bool(b) => format!("{k}={b}"),
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            lines.push_str(&format!(
                "{}{tags} {fields}",
                escape_line_part(&p.measurement)
            ));
            if let Some(ts) = p.timestamp {
                lines.push_str(&format!(" {ts}"));
            }
            lines.push('\n');
        }

        // 行协议构造（上面整个 `for p in points` 循环）是**纯本地**的，留在壳外：
        // 记账口径是「后端的表现」，本地字符串拼接不该进熔断窗口、也不该占超时预算。
        self.guarded(async {
            let resp = self
                .client
                .post(&self.write_url)
                .header("Authorization", format!("Token {}", self.token))
                .header("Content-Type", "text/plain; charset=utf-8")
                .query(&[
                    ("org", &self.org),
                    ("bucket", &self.bucket),
                    ("precision", &"ns".to_string()),
                ])
                .body(lines)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "influx", format!("write: {e}")))?;

            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "influx",
                    format!("write failed: {}", resp.text().await.unwrap_or_default()),
                ));
            }
            Ok(())
        })
        .await
    }

    async fn query(&self, query: &str) -> Result<serde_json::Value, Error> {
        // `query.to_string()` 是纯本地的，放壳里更省事，也不影响记账口径（这一步不发 HTTP）。
        self.guarded(async {
            let resp = self
                .client
                .post(&self.query_url)
                .header("Authorization", format!("Token {}", self.token))
                .header("Content-Type", "application/vnd.flux")
                .query(&[("org", &self.org)])
                .body(query.to_string())
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "influx", format!("query: {e}")))?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "influx",
                    format!("query failed: {}", resp.text().await.unwrap_or_default()),
                ));
            }
            resp.json()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "influx", format!("parse: {e}")))
        })
        .await
    }

    // `delete` 走 `TsdbClient` 的 trait 默认实现（`ecat-data/src/tsdb.rs:55`），
    // **不包 `guarded`**：默认实现的「不支持」是**调用方的用法错**，不是后端故障。
    // 包了之后 8 次「不支持」就会打开熔断器，之后**正常写入/查询全被拒绝**。守测试见
    // `tests/resilience.rs::delete_default_does_not_trip_the_breaker`。
    // `escape_line_part` / `escape_field_string` 是纯本地 helper，同样不进壳。
}

#[cfg(test)]
mod tests;

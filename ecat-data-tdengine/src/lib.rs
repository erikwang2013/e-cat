// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
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
pub struct TdengineConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub database: Option<String>,
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

pub struct TdengineClient {
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
    database: Option<String>,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
}

impl TdengineClient {
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
            database: None,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Some(Arc::new(Semaphore::new(32))),
        }
    }

    pub fn from_config(cfg: TdengineConfig) -> Result<Self, Error> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| Error::new(ErrorCode::Internal, "tdengine_tls", format!("TLS: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
            database: cfg.database,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
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
    ///
    /// **壳的边界 = 公开方法**：`write` 的分块循环里每批各发一次 HTTP，所以**整个
    /// 分块循环**在这一个壳内；私有的 `exec`（一次 POST + 解析）**不许再包一层** ——
    /// 那会把一次 `write` 切成 N 个独立预算（墙钟上限随分批数放大），且熔断窗口被
    /// 同一批数据记 N 次。判据：问「这一步发 HTTP 吗？」——
    /// `whole_call_budget_covers_every_batch_in_write` 盯着这条。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "tdengine"))
    }

    /// 纯本地拼 URL，不发 I/O —— **不包 `guarded`**（半开态下会把探测名额
    /// 以 `Ok` 白白消耗，见 `guarded` 的 rustdoc）。
    fn sql_url(&self) -> String {
        match &self.database {
            Some(db) => format!("{}/rest/sql/{}", self.base_url, percent_encode_segment(db)),
            None => format!("{}/rest/sql", self.base_url),
        }
    }

    async fn exec(&self, sql: &str) -> Result<serde_json::Value, Error> {
        let resp = self
            .client
            .post(self.sql_url())
            .basic_auth(&self.username, Some(&self.password))
            .json(&serde_json::json!({ "sql": sql }))
            .send()
            .await
            .map_err(|e| {
                Error::new(
                    ErrorCode::Internal,
                    "tdengine",
                    format!("tdengine exec: {e}"),
                )
            })?;
        if !resp.status().is_success() {
            return Err(Error::new(
                ErrorCode::Internal,
                "tdengine",
                resp.text().await.unwrap_or_default(),
            ));
        }
        resp.json().await.map_err(|e| {
            Error::new(
                ErrorCode::Internal,
                "tdengine",
                format!("tdengine parse: {e}"),
            )
        })
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

/// 转义双引号字符串字面量：先转义反斜杠再转义双引号，防止注入逃逸
fn escape_sql_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 转义双引号包裹的标识符（measurement/列名）
fn escape_ident(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 单条 DataPoint 生成一条 INSERT 语句
fn point_to_insert(p: &DataPoint) -> String {
    // Tags are flattened as columns; measurement is the table name.
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    cols.push("ts".to_string());
    vals.push(
        p.timestamp
            .map(|ts| ts.to_string())
            .unwrap_or_else(|| "now".to_string()),
    );
    for (k, v) in &p.tags {
        cols.push(format!("\"{}\"", escape_ident(k)));
        vals.push(format!("\"{}\"", escape_sql_string(v)));
    }
    for (k, v) in &p.fields {
        cols.push(format!("\"{}\"", escape_ident(k)));
        vals.push(match v {
            FieldValue::Float(f) => f.to_string(),
            FieldValue::Int(i) => i.to_string(),
            FieldValue::String(s) => format!("\"{}\"", escape_sql_string(s)),
            FieldValue::Bool(b) => {
                if *b {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
        });
    }
    format!(
        "INSERT INTO \"{}\" ({}) VALUES ({})",
        escape_ident(&p.measurement),
        cols.join(", "),
        vals.join(", ")
    )
}

/// 每批最多写入的语句数，TDengine REST 支持换行分隔的多语句
const BATCH_SIZE: usize = 100;

#[async_trait]
impl TsdbClient for TdengineClient {
    async fn write(&self, points: &[DataPoint]) -> Result<(), Error> {
        // 整个分块循环**一个预算**：`exec` 是内部 helper（一次 HTTP），
        // 它自己**不包** —— 包 `exec` 会把一次 `write` 切成 N 个独立预算，
        // 墙钟上限随分批数放大，而且熔断窗口会被同一批数据记 N 次。
        // （whole_call_budget_covers_every_batch_in_write 盯着这条。）
        self.guarded(async {
            for chunk in points.chunks(BATCH_SIZE) {
                let sql = chunk
                    .iter()
                    .map(point_to_insert)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.exec(&sql).await?;
            }
            Ok(())
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<serde_json::Value, Error> {
        self.guarded(async { self.exec(sql).await }).await
    }

    // `delete` 走 `TsdbClient` 的 trait 默认实现（`ecat-data/src/tsdb.rs:55`），
    // **不包 `guarded`**：默认实现的「不支持」是**调用方的用法错**，不是后端故障。
    // 包了之后 8 次「不支持」就会打开熔断器，之后**正常写入/查询全被拒绝**。守测试见
    // `tests/resilience.rs::delete_default_does_not_trip_the_breaker`。
}

#[cfg(test)]
mod tests;

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! ClickHouse 分析库客户端（`SqlExecutor` / `RdbmsClient` / `TsdbClient`）。
//!
//! # 出站韧性
//!
//! 两条做 I/O 的路径（`SqlExecutor` 的 `execute` / `query`，`TsdbClient` 的三个方法）
//! 都经过 `guarded` / `guarded_tsdb`：**许可 → 熔断 → 超时**（顺序理由见批次 5a
//! 计划的「与 spec 的出入 4」）。两条路径**共用一个** `Breaker` —— 同一个服务器、
//! 同一个故障域。`transaction()`（常量错误、不含 I/O）与 `_with` 系列（落到 trait
//! 默认实现）**不包**，见「出入 6/7」。
//!
//! # 超时分两层
//!
//! 本 client 的 `query_timeout_secs` 是外层预算；[`ecat_tls::build_reqwest_client`]
//! 建出的连接另外自带 reqwest 的 5 秒连接超时 + 30 秒总超时。`from_config` 走的是
//! 后者，所以实际预算是**两层取先到者**（`new` / `with_auth` 用裸
//! `reqwest::Client::new()`，没有内层超时）。
mod tsdb;

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, DataPoint, Dialect, FieldValue, RdbmsClient, RdbmsError, Row, SqlExecutor,
    breaker_error_to_backend_error, run_with_timeout,
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
pub struct ClickhouseConfig {
    pub base_url: String,
    #[serde(default = "default_database")]
    pub database: String,
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
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。reqwest **只有** `pool_max_idle_per_host`（空闲保留数），
    /// 没有「最大总连接数」—— 默认无上限意味着并发无背压。
    /// 未配置 = 32。上限由本 crate 的信号量实现，不是 reqwest 的旋钮。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

fn default_database() -> String {
    "default".into()
}

/// 建表缓存 TTL：外部 drop/改表后，超过 TTL 的下一次写入会重新 CREATE。
const CREATE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

pub struct ClickhouseClient {
    client: reqwest::Client,
    base_url: String,
    database: String,
    username: Option<String>,
    password: Option<String>,
    // 建表缓存：每 client 一份，避免跨 client/database 误跳过建表；
    // 记录建表时间，超过 create_ttl 后重新 CREATE（CREATE IF NOT EXISTS 幂等）。
    created: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    create_ttl: std::time::Duration,
    query_timeout: Option<Duration>,
    /// 两条 I/O 路径（`SqlExecutor` / `TsdbClient`）**共用**一个 ——
    /// 同一个服务器、同一个故障域，两条路径各判一次会把故障域切错。
    breaker: Arc<Breaker>,
    semaphore: Arc<Semaphore>,
}

/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

impl ClickhouseClient {
    pub fn new(base_url: impl Into<String>, database: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            database: database.into(),
            username: None,
            password: None,
            created: std::sync::Mutex::new(std::collections::HashMap::new()),
            create_ttl: CREATE_TTL,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Arc::new(Semaphore::new(32)),
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        database: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            database: database.into(),
            username: Some(username.into()),
            password: Some(password.into()),
            created: std::sync::Mutex::new(std::collections::HashMap::new()),
            create_ttl: CREATE_TTL,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Arc::new(Semaphore::new(32)),
        }
    }

    pub fn from_config(cfg: ClickhouseConfig) -> Result<Self, RdbmsError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| RdbmsError::Config(format!("TLS: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
            database: cfg.database,
            username: cfg.username,
            password: cfg.password,
            created: std::sync::Mutex::new(std::collections::HashMap::new()),
            create_ttl: CREATE_TTL,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32))),
        })
    }

    /// 本 client 的熔断器（`metrics` feature 用）。
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

    /// 一次出站调用的外壳（`RdbmsError` 路径）。
    ///
    /// **顺序：许可 → 熔断 → 超时**（理由见批次 5a 计划的「与 spec 的出入 4」）：
    /// - 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断
    /// - 熔断在超时外：超时是一次普通的 `Err`，会**如实计入**熔断窗口 ——
    ///   否则卡死的后端永远打不开熔断器
    pub(crate) async fn guarded<F, T: 'static>(
        &self,
        kind: BackendKind,
        fut: F,
    ) -> Result<T, RdbmsError>
    where
        F: std::future::Future<Output = Result<T, RdbmsError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(kind, self.query_timeout, fut))
            .await
            .map_err(ecat_data::map_breaker_error)
    }

    /// 同上的 `ecat_errors::Error` 路径（`TsdbClient`）。
    pub(crate) async fn guarded_tsdb<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "clickhouse"))
    }

    /// 建表缓存缺失或已过期（需要重新 CREATE）。
    fn table_needs_create(&self, measurement: &str) -> bool {
        let created = self.created.lock().unwrap_or_else(|e| e.into_inner());
        match created.get(measurement) {
            Some(at) => at.elapsed() >= self.create_ttl,
            None => true,
        }
    }

    /// 执行 CREATE TABLE IF NOT EXISTS 并刷新缓存；失败不缓存，下次调用重试。
    async fn create_table(
        &self,
        measurement: &str,
        tag_keys: &[String],
        field_cols: &[(String, &'static str)],
    ) -> Result<(), Error> {
        let create = build_create_table(measurement, tag_keys, field_cols);
        let resp = self.post(&create, &[]).send().await.map_err(|e| {
            Error::new(ErrorCode::Internal, "clickhouse", format!("ch create: {e}"))
        })?;
        if !resp.status().is_success() {
            return Err(Error::new(
                ErrorCode::Internal,
                "clickhouse",
                format!(
                    "ch create failed: {}",
                    resp.text().await.unwrap_or_default()
                ),
            ));
        }
        self.created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(measurement.to_string(), std::time::Instant::now());
        Ok(())
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        ecat_tls::apply_basic_auth(req, &self.username, &self.password)
    }

    fn post(&self, sql: &str, params: &[(&str, String)]) -> reqwest::RequestBuilder {
        let mut rb = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "text/plain; charset=utf-8")
            .query(&[("database", self.database.clone())])
            .body(sql.to_string());
        for (k, v) in params {
            rb = rb.query(&[(*k, v.clone())]);
        }
        self.apply_auth(rb)
    }
}

fn quote_ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

fn field_type(v: &FieldValue) -> &'static str {
    match v {
        FieldValue::Float(_) => "Float64",
        FieldValue::Int(_) => "Int64",
        FieldValue::String(_) => "String",
        FieldValue::Bool(_) => "UInt8",
    }
}

fn field_to_json(v: &FieldValue) -> serde_json::Value {
    match v {
        // 非有限浮点（NaN/±Inf）无法用 JSON number 表示：serde_json 的 from_f64
        // 对非有限值返回 None，且 ClickHouse 的 JSONEachRow 也没有 NaN/Inf 字面量。
        // 因此回退为 0 保证序列化不失败；调用方应在写入前自行清洗 NaN/Inf。
        FieldValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Number(0.into())),
        FieldValue::Int(i) => serde_json::Value::Number((*i).into()),
        FieldValue::String(s) => serde_json::Value::String(s.clone()),
        FieldValue::Bool(b) => serde_json::Value::Bool(*b),
    }
}

fn build_create_table(
    measurement: &str,
    tag_keys: &[String],
    field_cols: &[(String, &'static str)],
) -> String {
    let mut cols: Vec<String> = tag_keys
        .iter()
        .map(|k| format!("{} String", quote_ident(k)))
        .collect();
    cols.extend(
        field_cols
            .iter()
            .map(|(k, ty)| format!("{} {ty}", quote_ident(k))),
    );
    cols.push("`timestamp` Int64 DEFAULT 0".into());
    format!(
        "CREATE TABLE IF NOT EXISTS {} ({}) ENGINE = MergeTree ORDER BY timestamp",
        quote_ident(measurement),
        cols.join(", ")
    )
}

fn build_insert_body(points: &[&DataPoint], tag_keys: &[String], field_keys: &[String]) -> String {
    // 逐点按 tags → fields → timestamp 顺序手写 JSON 对象，保证输出列序与
    // INSERT 语句列序一致（serde_json 的 Map 默认按 key 排序，不依赖其 preserve_order 特性）。
    // 键的引号序列化与列序无关且全批一致，预计算一次跨点复用。
    let tag_quoted: Vec<(String, String)> = tag_keys
        .iter()
        .map(|k| (k.clone(), serde_json::to_string(k).unwrap()))
        .collect();
    let field_quoted: Vec<(String, String)> = field_keys
        .iter()
        .map(|k| (k.clone(), serde_json::to_string(k).unwrap()))
        .collect();
    let ts_quoted = serde_json::to_string("timestamp").unwrap();
    let mut out = String::new();
    for p in points {
        let mut parts: Vec<String> = Vec::new();
        for (k, qk) in &tag_quoted {
            if let Some(v) = p.tags.get(k) {
                parts.push(format!("{qk}:{}", serde_json::to_string(v).unwrap()));
            }
        }
        for (k, qk) in &field_quoted {
            if let Some(v) = p.fields.get(k) {
                parts.push(format!(
                    "{qk}:{}",
                    serde_json::to_string(&field_to_json(v)).unwrap()
                ));
            }
        }
        if let Some(ts) = p.timestamp {
            parts.push(format!("{ts_quoted}:{ts}"));
        }
        out.push('{');
        out.push_str(&parts.join(","));
        out.push_str("}\n");
    }
    out
}

#[async_trait]
impl SqlExecutor for ClickhouseClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.guarded(BackendKind::Rdbms, async {
            let resp = self
                .post(sql, &[("send_progress_in_http_headers", "1".to_string())])
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("ch: {e}")))?;
            if !resp.status().is_success() {
                return Err(RdbmsError::Database(resp.text().await.unwrap_or_default()));
            }
            // ClickHouse reports written/result rows in the X-ClickHouse-Summary
            // response header (enabled via send_progress_in_http_headers=1).
            // Falls back to 0 when the server does not send the header.
            let affected = resp
                .headers()
                .get("x-clickhouse-summary")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| {
                    v.get("written_rows")
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            Ok(affected)
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.guarded(BackendKind::Rdbms, async {
            let resp = self
                .post(sql, &[("default_format", "JSONEachRow".to_string())])
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("ch query: {e}")))?;
            let text = resp
                .text()
                .await
                .map_err(|e| RdbmsError::Database(format!("ch read: {e}")))?;
            let mut rows = Vec::new();
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let v = serde_json::from_str::<serde_json::Value>(line).map_err(|e| {
                    RdbmsError::Database(format!(
                        "ch query: unparseable row (first 200 bytes shown): {e}: {}",
                        line.chars().take(200).collect::<String>()
                    ))
                })?;
                if let Some(obj) = v.as_object() {
                    let cols: Vec<String> = obj.keys().cloned().collect();
                    let vals: Vec<serde_json::Value> = obj.values().cloned().collect();
                    rows.push(Row::new(cols, vals));
                }
            }
            Ok(rows)
        })
        .await
    }

    /// ClickHouse SQL 无 `Dialect` 对应变体，按文档契约回退到 [`Dialect::Standard`]。
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for ClickhouseClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "ClickHouse does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests;

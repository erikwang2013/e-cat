// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Druid SQL over HTTP (`POST /druid/v2/sql`).
//!
//! 官方文档：
//! - SQL API：<https://druid.apache.org/docs/latest/api-reference/sql-api/>
//! - Basic 认证：<https://druid.apache.org/docs/latest/development/extensions-core/druid-basic-security>
//!
//! 成功响应是对象数组（默认 `resultFormat: object`），列名 = 首个对象的 key 顺序。
//! 400/500 的错误体为 `{"error", "errorMessage", "errorClass", "host"}`；
//! 错误文案优先取 `errorMessage`，其次 `error`，都没有则回退到响应体原文。
//! 凭据只走 `Authorization: Basic` 头，不会出现在 URL 里。

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, map_breaker_error,
    run_with_timeout,
};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

#[derive(Debug, Clone, Deserialize)]
pub struct DruidConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 合并进每个请求体的 Druid SQL context 参数；省略/空对象 = 不发。
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒、`new` / `with_auth` 没有）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    ///
    /// 熔断**默认开启**，没有总开关：要停用只能把阈值调到不可能触发
    /// （如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = **不限并发**；未配置 = 32。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

pub struct DruidClient {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    context: HashMap<String, serde_json::Value>,
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

/// 保留 JSON 对象键的**文档顺序**。
///
/// 未开 `serde_json/preserve_order` 时 `Map` 是 `BTreeMap`（按字母序），
/// 而 Druid 的列顺序就是首个对象的 key 顺序 —— 排序会静默换列。
struct OrderedObject(Vec<(String, serde_json::Value)>);

impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OrderedObject;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, serde_json::Value>()? {
                    entries.push(entry);
                }
                Ok(OrderedObject(entries))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

/// 错误体里的 `errorMessage`，其次 `error`，都没有则用原文。
fn druid_error(body: &str) -> RdbmsError {
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("errorMessage")
                .and_then(|m| m.as_str())
                .or_else(|| v.get("error").and_then(|m| m.as_str()))
                .filter(|m| !m.is_empty())
                .map(str::to_string)
        });
    RdbmsError::Database(msg.unwrap_or_else(|| body.to_string()))
}

/// 2xx 但不是数组的响应体若带错误字段，按错误处理。
/// （Druid 用 4xx/5xx 报错；这里兼容网关透传的 200 + error 体。）
fn api_error(body: &str) -> Option<RdbmsError> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let has_error = value.get("error").is_some() || value.get("errorMessage").is_some();
    has_error.then(|| druid_error(trimmed))
}

/// 行 = 对象；列名取**首个对象**的 key 顺序，后续行缺列补 `null`。
fn parse_rows(body: &str) -> Result<Vec<Row>, RdbmsError> {
    let objects: Vec<OrderedObject> = serde_json::from_str(body)
        .map_err(|e| RdbmsError::Database(format!("druid parse: {e}")))?;
    let Some(first) = objects.first() else {
        return Ok(Vec::new());
    };
    let columns: Vec<String> = first.0.iter().map(|(k, _)| k.clone()).collect();
    Ok(objects
        .into_iter()
        .map(|obj| {
            let values = columns
                .iter()
                .map(|col| {
                    obj.0
                        .iter()
                        .find(|(k, _)| k == col)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(serde_json::Value::Null)
                })
                .collect();
            Row::new(columns.clone(), values)
        })
        .collect())
}

impl DruidClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            username: None,
            password: None,
            context: HashMap::new(),
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
            username: Some(username.into()),
            password: Some(password.into()),
            ..Self::new(base_url)
        }
    }

    pub fn from_config(cfg: DruidConfig) -> Result<Self, RdbmsError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| RdbmsError::Config(format!("TLS: {e}")))?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // `metrics` feature 下构造即注册；`register_outbound_metrics` 幂等。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            username: cfg.username,
            password: cfg.password,
            context: cfg.context,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
            semaphore: match cfg.max_concurrency {
                // `0` = 不限并发：不建信号量。`Semaphore::new(0)` 会让每次调用
                // 静默挂在 `permit().await` 上。
                Some(0) => None,
                Some(n) => Some(Arc::new(Semaphore::new(n))),
                None => Some(Arc::new(Semaphore::new(32))),
            },
        })
    }

    /// 本 client 的熔断器（`metrics` feature 注册指标时读它的状态）。
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
    /// 顺序不能改：超时若在外层，熔断器会对卡死的后端永久失明（超时 drop 内层
    /// future，记失败的那句走不到）。许可在最外：排队的请求还没碰后端。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, RdbmsError>
    where
        F: std::future::Future<Output = Result<T, RdbmsError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Rdbms, self.query_timeout, fut))
            .await
            .map_err(map_breaker_error)
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        ecat_tls::apply_basic_auth(req, &self.username, &self.password)
    }

    /// `{"query": sql}`，配置了非空 context 时合并进去。
    fn request_body(&self, sql: &str) -> serde_json::Value {
        let mut body = serde_json::json!({ "query": sql });
        if !self.context.is_empty() {
            body["context"] = serde_json::Value::Object(self.context.clone().into_iter().collect());
        }
        body
    }

    /// 发一次 SQL 请求并返回成功响应体；HTTP 错误与 2xx 错误体都归一成 `RdbmsError`。
    async fn post_sql(&self, sql: &str) -> Result<String, RdbmsError> {
        // 请求构造是纯本地的，留在壳外；只有 `send()` 起才是 I/O。
        let req = self
            .client
            .post(format!("{}/druid/v2/sql", self.base_url))
            .json(&self.request_body(sql));
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("druid: {e}")))?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_else(|e| format!("druid: {e}"));
            if !status.is_success() {
                return Err(druid_error(&text));
            }
            if let Some(err) = api_error(&text) {
                return Err(err);
            }
            Ok(text)
        })
        .await
    }
}

#[async_trait]
impl SqlExecutor for DruidClient {
    /// Druid SQL 以 SELECT 为主，HTTP API 不返回受影响行数，按契约固定回 0。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.post_sql(sql).await.map(|_| 0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        parse_rows(&self.post_sql(sql).await?)
    }

    /// Druid SQL 为 Calcite 方言，`Dialect` 无对应变体，按文档契约回退 Standard。
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for DruidClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "Druid does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests;

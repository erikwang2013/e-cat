// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Kylin query REST API (`POST /kylin/api/query`)。
//!
//! **目标版本：Kylin 3.1.x**（平铺响应），文档：
//! - Query API：<https://kylin.apache.org/docs/3.1.3/howto/howto_use_restapi/>
//! - 4/5 的 v4-public envelope：<https://kylin.apache.org/docs/restapi/query_api/>
//!
//! 请求体：`{"sql", "offset": 0, "limit": 50000, "project"?}`；
//! 响应：`columnMetas:[{name,...}]` + `results:[[...]]`，`isException: true` 时用
//! `exceptionMessage` 报错。Kylin 4/5 的 v4-public 响应把同一份 payload 包在
//! `{"code","data","msg"}` 的 `data` 下，这里也一并解包。
//!
//! 认证走 HTTP Basic（`Authorization: Basic`），官方示例见上面的 Query API 文档。
//! 服务端在 SQL 自带 `LIMIT` 时忽略请求体里的 `limit`。

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, map_breaker_error,
    run_with_timeout,
};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

/// 请求体的默认 `limit`（官方示例取 50000）；SQL 里写 `LIMIT` 时服务端忽略它。
const DEFAULT_LIMIT: usize = 50_000;

#[derive(Debug, Clone, Deserialize)]
pub struct KylinConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// Kylin 项目名。Kylin 3.x 省略时服务端默认 `DEFAULT`；
    /// Kylin 4/5 的 v4-public API 要求必填。
    #[serde(default)]
    pub project: Option<String>,
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

pub struct KylinClient {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    project: Option<String>,
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

/// 解析 Kylin 3.x 平铺响应，或 Kylin 4/5 `data` envelope 里的同一份 payload。
fn parse_rows(body: &str) -> Result<Vec<Row>, RdbmsError> {
    let root: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| RdbmsError::Database(format!("kylin parse: {e}")))?;
    let data = if root.get("columnMetas").is_some() || root.get("results").is_some() {
        &root
    } else {
        // v4-public envelope 的错误也是 200 + 非 "000" code；
        // 解包成空行集等于静默吞错。
        if let Some(code) = root
            .get("code")
            .and_then(serde_json::Value::as_str)
            .filter(|c| *c != "000")
        {
            let msg = root
                .get("msg")
                .and_then(serde_json::Value::as_str)
                .filter(|m| !m.is_empty())
                .unwrap_or("Kylin query failed");
            return Err(RdbmsError::Database(format!("{msg} (code {code})")));
        }
        root.get("data").unwrap_or(&root)
    };
    if data.get("isException").and_then(serde_json::Value::as_bool) == Some(true) {
        let msg = data
            .get("exceptionMessage")
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or("Kylin query failed");
        return Err(RdbmsError::Database(msg.to_string()));
    }
    let Some(results) = data.get("results").and_then(serde_json::Value::as_array) else {
        return Ok(Vec::new());
    };
    let Some(metas) = data
        .get("columnMetas")
        .and_then(serde_json::Value::as_array)
    else {
        if results.is_empty() {
            return Ok(Vec::new());
        }
        return Err(RdbmsError::Database(
            "kylin parse: results without columnMetas".into(),
        ));
    };
    let columns: Vec<String> = metas
        .iter()
        .map(|m| {
            m.get("name")
                .or_else(|| m.get("label"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let mut rows = Vec::with_capacity(results.len());
    for result in results {
        // 畸形行（不是数组）跳过，与 QuestDB client 同策略。
        let Some(values) = result.as_array() else {
            continue;
        };
        let mut values = values.clone();
        values.resize(columns.len(), serde_json::Value::Null);
        values.truncate(columns.len());
        rows.push(Row::new(columns.clone(), values));
    }
    Ok(rows)
}

impl KylinClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            username: None,
            password: None,
            project: None,
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

    pub fn from_config(cfg: KylinConfig) -> Result<Self, RdbmsError> {
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
            project: cfg.project,
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

    fn request_body(&self, sql: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "sql": sql,
            "offset": 0,
            "limit": DEFAULT_LIMIT,
        });
        if let Some(project) = &self.project {
            body["project"] = serde_json::Value::String(project.clone());
        }
        body
    }

    /// 发一次查询并返回成功响应体；4xx/5xx 的响应体原文作为错误。
    async fn post_sql(&self, sql: &str) -> Result<String, RdbmsError> {
        // 请求构造是纯本地的，留在壳外；只有 `send()` 起才是 I/O。
        let req = self
            .client
            .post(format!("{}/kylin/api/query", self.base_url))
            .json(&self.request_body(sql));
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("kylin: {e}")))?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_else(|e| format!("kylin: {e}"));
            if !status.is_success() {
                return Err(RdbmsError::Database(text));
            }
            Ok(text)
        })
        .await
    }
}

#[async_trait]
impl SqlExecutor for KylinClient {
    /// Kylin 的 `/query` 端点面向 SELECT，不返回受影响行数；DDL/写入应走 Kylin
    /// 的其他管理 API。按 `SqlExecutor` 契约固定返回 0，同时仍会解包
    /// `isException`（查询失败不能被吞成成功）。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        parse_rows(&self.post_sql(sql).await?)?;
        Ok(0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        parse_rows(&self.post_sql(sql).await?)
    }

    /// Kylin SQL 接近 Calcite/ANSI，`Dialect` 无对应变体，按文档契约回退 Standard。
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for KylinClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "Kylin does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests;

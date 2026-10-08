// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, DocumentClient, breaker_error_to_backend_error, run_with_timeout};
use ecat_errors::{Error, ErrorCode};
use ecat_tls::TlsClientConfig;
use futures_util::TryStreamExt;
use mongodb::bson;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

#[derive(Debug, Clone, Deserialize)]
pub struct MongoConfig {
    pub url: String,
    pub database: String,
    // TODO(5c): 未接线（spec 未要求；mongodb 3.x 的 TLS 走 URI 选项）
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次命令超时秒数。`0` = 禁用；未配置 = 30 秒。
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
    /// 连接池上限。未配置 = 驱动默认（mongodb 3.8.0 实测 **10**，`src/cmap.rs:50`；
    /// **不是** spec §5 写的 100 —— 那是 Node 驱动的默认值）。
    #[serde(default)]
    pub max_pool_size: Option<u32>,
    /// 连接池下限（后台保活连接数）。未配置 = 驱动默认 0。
    #[serde(default)]
    pub min_pool_size: Option<u32>,
}

/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

pub struct MongoClient {
    client: mongodb::Client,
    database: String,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
}

impl MongoClient {
    /// 从配置建 `ClientOptions`。**单独成函数是为了可测**：`mongodb://` URI 的解析
    /// 不发网络请求，所以池大小的接线能在没有服务器的单测里被断言到
    /// （删掉下面两行赋值，`config_wires_timeout_pool_and_breaker` 立刻红）。
    async fn build_options(cfg: &MongoConfig) -> Result<mongodb::options::ClientOptions, Error> {
        // `ClientOptions::parse` 不是 async fn，返回的是可 await 的 action builder
        // （mongodb 3.8.0 `src/action/client_options.rs:68-79`）。
        let mut options = mongodb::options::ClientOptions::parse(&cfg.url)
            .await
            .map_err(|e| {
                Error::new(
                    ErrorCode::Internal,
                    "mongodb",
                    format!("mongodb connect: {e}"),
                )
            })?;
        // 池大小走**驱动的旋钮**：本 crate 没有信号量。`None` = 不覆盖，交给驱动默认。
        options.max_pool_size = cfg.max_pool_size;
        options.min_pool_size = cfg.min_pool_size;
        Ok(options)
    }

    pub async fn from_config(cfg: MongoConfig) -> Result<Self, Error> {
        let options = Self::build_options(&cfg).await?;
        let client = mongodb::Client::with_options(options).map_err(|e| {
            Error::new(
                ErrorCode::Internal,
                "mongodb",
                format!("mongodb connect: {e}"),
            )
        })?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线（lead 裁决 2026-10-08）：`metrics` feature 下**构造即注册**，
        // 用户代码零变化。`ecat_metrics::register_outbound_metrics` 是幂等的
        // （同一 backend 重复注册是覆盖闭包），所以多 client 不会炸。
        // 探针：注释掉下面两行 ⇒ from_config_registers_outbound_metrics 红。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            client,
            database: cfg.database,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
        })
    }

    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 一次出站调用的外壳：**熔断 → 超时**（**没有许可层** —— 并发背压交给
    /// 驱动连接池 `max_pool_size`，见 spec §5）。
    ///
    /// 这个顺序不能改：**超时若在外层，熔断器会对卡死的后端永久失明** ——
    /// 超时触发时 `tokio::time::timeout` 会 drop 内层 future，而熔断器记失败的那句
    /// 在 `f().await` **之后**，于是每次都只留下一次 drop、窗口里什么都不记，
    /// 熔断器永远不打开。详见批次 5a 计划的「出入 4」。
    ///
    /// `kind` **写死**不收参数：本 crate 的四个 I/O 方法同属 `DocumentClient` 一个家族。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        self.breaker
            .call(|| run_with_timeout(BackendKind::Document, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "mongodb"))
    }
}

#[async_trait]
impl DocumentClient for MongoClient {
    async fn insert(&self, collection: &str, doc: &Value) -> Result<String, Error> {
        // bson 转换是**调用方的输入错误**（不是后端故障），留在外壳外面 ——
        // 否则 5 次「传了非文档」就会把熔断器打开、之后正常写入全被拒绝。
        let doc = bson::to_document(doc).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let result = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .insert_one(doc)
                .await
                .map_err(|e| {
                    Error::new(
                        ErrorCode::Internal,
                        "mongodb",
                        format!("mongodb insert: {e}"),
                    )
                })?;
            Ok(result.inserted_id.to_string())
        })
        .await
    }

    async fn find(&self, collection: &str, filter: &Value) -> Result<Vec<Value>, Error> {
        let filter = bson::to_document(filter).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let cursor = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .find(filter)
                .await
                .map_err(|e| {
                    Error::new(ErrorCode::Internal, "mongodb", format!("mongodb find: {e}"))
                })?;
            let docs: Vec<bson::Document> = cursor.try_collect().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "mongodb", format!("mongodb find: {e}"))
            })?;
            docs.iter()
                .map(|d| {
                    serde_json::to_value(d).map_err(|e| {
                        Error::new(ErrorCode::Internal, "mongodb", format!("mongodb json: {e}"))
                    })
                })
                .collect()
        })
        .await
    }

    async fn update(&self, collection: &str, filter: &Value, update: &Value) -> Result<u64, Error> {
        let filter = bson::to_document(filter).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        let update = bson::to_document(update).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let result = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .update_many(filter, update)
                .await
                .map_err(|e| {
                    Error::new(
                        ErrorCode::Internal,
                        "mongodb",
                        format!("mongodb update: {e}"),
                    )
                })?;
            Ok(result.modified_count)
        })
        .await
    }

    async fn delete(&self, collection: &str, filter: &Value) -> Result<u64, Error> {
        let filter = bson::to_document(filter).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let result = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .delete_many(filter)
                .await
                .map_err(|e| {
                    Error::new(
                        ErrorCode::Internal,
                        "mongodb",
                        format!("mongodb delete: {e}"),
                    )
                })?;
            Ok(result.deleted_count)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    mod resilience;

    use super::*;

    #[test]
    fn config_deserializes() {
        let cfg: MongoConfig = serde_json::from_value(serde_json::json!({
            "url": "mongodb://localhost:27017",
            "database": "app",
        }))
        .unwrap();
        assert_eq!(cfg.database, "app");
    }

    #[tokio::test]
    async fn from_config_rejects_bad_uri() {
        let result = MongoClient::from_config(MongoConfig {
            url: "not-a-valid-uri".into(),
            database: "app".into(),
            tls: None,
            query_timeout_secs: None,
            breaker: None,
            max_pool_size: None,
            min_pool_size: None,
        })
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn config_missing_url_or_database_is_error() {
        assert!(serde_json::from_str::<MongoConfig>(r#"{"database":"app"}"#).is_err());
        assert!(
            serde_json::from_str::<MongoConfig>(r#"{"url":"mongodb://localhost:27017"}"#).is_err()
        );
    }

    /// serde_json i64 → bson i64 → serde_json 的全程保真：超过 f64 精确表示
    /// 范围的整数不得被 float 转换截断（如 ObjectId 式大 id / 时间戳）。
    #[test]
    fn bson_roundtrip_preserves_large_i64_precision() {
        let v: Value = serde_json::json!({"big": 9_007_199_254_740_993_i64});
        let doc = bson::to_document(&v).unwrap();
        let back: Value = serde_json::to_value(doc).unwrap();
        assert_eq!(back["big"], serde_json::json!(9_007_199_254_740_993_i64));
    }

    #[test]
    fn bson_roundtrip_preserves_nested_and_negative() {
        let v: Value = serde_json::json!({
            "neg": -42,
            "f": -0.25,
            "arr": [1, "two", null],
            "deep": {"a": {"b": {"c": true}}},
        });
        let doc = bson::to_document(&v).unwrap();
        let back: Value = serde_json::to_value(doc).unwrap();
        assert_eq!(v, back);
    }

    /// insert/find/update/delete 的公共输入路径：serde_json::Value →
    /// bson::Document 往返保真（嵌套、数组、各标量类型、null）。
    #[test]
    fn bson_roundtrip_preserves_json_object() {
        let v: Value = serde_json::json!({
            "name": "alice",
            "age": 30,
            "active": true,
            "score": 1.5,
            "tags": ["a", "b", "c"],
            "nested": {"level": 2, "nil": null},
            "none": null,
        });
        let doc = bson::to_document(&v).unwrap();
        let back: Value = serde_json::to_value(doc).unwrap();
        assert_eq!(v, back);
    }

    /// bson::to_document 要求顶层为文档：null / 数组等非文档值必须报错
    /// （否则 insert 会拿非法文档直达网络）。
    #[test]
    fn bson_to_document_rejects_non_document_top_level() {
        assert!(bson::to_document(&Value::Null).is_err());
        assert!(bson::to_document(&serde_json::json!([1, 2, 3])).is_err());
        assert!(bson::to_document(&Value::from("str")).is_err());
    }

    /// 错误路径先于网络访问：insert 的 bson 转换失败返回 Error，
    /// 不发起任何连接（url 指向不可达端口也无妨）。
    #[tokio::test]
    async fn insert_rejects_non_document_before_network() {
        let client = MongoClient::from_config(MongoConfig {
            url: "mongodb://127.0.0.1:1".into(),
            database: "app".into(),
            tls: None,
            query_timeout_secs: None,
            breaker: None,
            max_pool_size: None,
            min_pool_size: None,
        })
        .await
        .unwrap();
        let err = client.insert("col", &Value::Null).await.unwrap_err();
        assert!(err.to_string().contains("mongodb bson:"), "got: {err}");
    }
}

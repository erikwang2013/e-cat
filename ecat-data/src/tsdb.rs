use async_trait::async_trait;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct DataPoint {
    pub measurement: String,
    pub tags: HashMap<String, String>,
    pub fields: HashMap<String, f64>,
    pub timestamp: Option<i64>,
}

#[async_trait]
pub trait TsdbClient: Send + Sync {
    async fn write(&self, points: &[DataPoint]) -> Result<(), TsdbError>;
    async fn query(&self, query: &str) -> Result<serde_json::Value, TsdbError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TsdbError {
    #[error("tsdb error: {0}")]
    Other(String),
}

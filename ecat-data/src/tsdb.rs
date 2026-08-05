// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum FieldValue {
    Float(f64),
    Int(i64),
    String(String),
    Bool(bool),
}

#[derive(Debug, Clone)]
pub struct DataPoint {
    pub measurement: String,
    pub tags: HashMap<String, String>,
    pub fields: HashMap<String, FieldValue>,
    pub timestamp: Option<i64>,
}

impl DataPoint {
    pub fn new(measurement: impl Into<String>) -> Self {
        Self {
            measurement: measurement.into(),
            tags: HashMap::new(),
            fields: HashMap::new(),
            timestamp: None,
        }
    }

    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }

    pub fn with_field(mut self, key: impl Into<String>, value: FieldValue) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    pub fn with_timestamp(mut self, ts: i64) -> Self {
        self.timestamp = Some(ts);
        self
    }
}

#[async_trait]
pub trait TsdbClient: Send + Sync {
    async fn write(&self, points: &[DataPoint]) -> Result<(), TsdbError>;
    async fn query(&self, query: &str) -> Result<serde_json::Value, TsdbError>;

    /// Delete data using a backend-specific query (e.g. `DELETE FROM ...`).
    /// Backends that cannot delete return an error.
    async fn delete(&self, _query: &str) -> Result<(), TsdbError> {
        Err(TsdbError::Other(
            "delete not supported by this backend".into(),
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TsdbError {
    #[error("tsdb error: {0}")]
    Other(String),
}

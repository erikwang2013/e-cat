mod env;
mod file;

pub use env::EnvSource;
pub use file::FileSource;

use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;

#[async_trait]
pub trait ConfigSource: Send + Sync {
    async fn load(&self) -> Result<HashMap<String, serde_json::Value>, ConfigError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config error: {0}")]
    Other(String),
}

pub struct Config {
    data: HashMap<String, serde_json::Value>,
}

impl Config {
    pub fn new() -> Self {
        Self { data: HashMap::new() }
    }

    pub async fn load(&mut self, source: &dyn ConfigSource) -> Result<(), ConfigError> {
        let values = source.load().await?;
        self.data.extend(values);
        Ok(())
    }

    pub fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        self.data.get(key).and_then(|v| T::deserialize(v).ok())
    }
}

use super::{ConfigError, ConfigSource};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl ConfigSource for FileSource {
    async fn load(&self) -> Result<HashMap<String, serde_json::Value>, ConfigError> {
        let content = tokio::fs::read_to_string(&self.path).await.map_err(|e| {
            ConfigError::Other(format!("read {}: {}", self.path.display(), e))
        })?;

        let value: serde_json::Value = if self.path.extension().map_or(false, |e| e == "yaml" || e == "yml") {
            serde_yaml::from_str(&content).map_err(|e| ConfigError::Other(e.to_string()))?
        } else {
            serde_json::from_str(&content).map_err(|e| ConfigError::Other(e.to_string()))?
        };

        let map = value.as_object().cloned().unwrap_or_default();
        Ok(map.into_iter().map(|(k, v)| (k, v)).collect())
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::{ConfigError, ConfigSource};
use async_trait::async_trait;
use std::collections::HashMap;

pub struct EnvSource {
    prefix: String,
}

impl EnvSource {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self { prefix: prefix.into() }
    }
}

#[async_trait]
impl ConfigSource for EnvSource {
    async fn load(&self) -> Result<HashMap<String, serde_json::Value>, ConfigError> {
        let mut map = HashMap::new();
        for (key, value) in std::env::vars() {
            if key.starts_with(&self.prefix) {
                let k = key[self.prefix.len()..].to_lowercase();
                map.insert(k, serde_json::Value::String(value));
            }
        }
        Ok(map)
    }
}

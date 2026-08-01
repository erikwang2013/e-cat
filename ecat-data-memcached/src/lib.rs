// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{Cache, CacheError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

type CacheEntry = (Vec<u8>, Option<Instant>);

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MemcachedConfig {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// TLS config — reserved for future network-based memcached implementation.
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct MemcachedClient {
    store: Mutex<HashMap<Vec<u8>, CacheEntry>>,
    #[allow(dead_code)]
    username: Option<String>,
    #[allow(dead_code)]
    password: Option<String>,
}

impl MemcachedClient {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            username: None,
            password: None,
        }
    }

    pub fn with_auth(
        _username: impl Into<String>,
        _password: impl Into<String>,
    ) -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            username: Some(_username.into()),
            password: Some(_password.into()),
        }
    }

    pub fn from_config(cfg: MemcachedConfig) -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            username: cfg.username,
            password: cfg.password,
        }
    }
}

impl Default for MemcachedClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Cache for MemcachedClient {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        let store = self
            .store
            .lock()
            .map_err(|e| CacheError::Other(e.to_string()))?;
        match store.get(key.as_bytes()) {
            Some((val, Some(exp))) if Instant::now() > *exp => Ok(None),
            Some((val, _)) => Ok(Some(val.clone())),
            None => Ok(None),
        }
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), CacheError> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| CacheError::Other(e.to_string()))?;
        let expires = if ttl.is_zero() {
            None
        } else {
            Some(Instant::now() + ttl)
        };
        store.insert(key.as_bytes().to_vec(), (value.to_vec(), expires));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| CacheError::Other(e.to_string()))?;
        store.remove(key.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get() {
        let c = MemcachedClient::new();
        c.set("k", b"v", Duration::from_secs(60)).await.unwrap();
        assert_eq!(c.get("k").await.unwrap(), Some(b"v".to_vec()));
    }

    #[tokio::test]
    async fn get_missing() {
        let c = MemcachedClient::new();
        assert_eq!(c.get("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn delete_removes() {
        let c = MemcachedClient::new();
        c.set("x", b"y", Duration::from_secs(60)).await.unwrap();
        c.delete("x").await.unwrap();
        assert_eq!(c.get("x").await.unwrap(), None);
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use std::time::Duration;

#[async_trait]
pub trait Cache: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError>;
    async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), CacheError>;
    async fn delete(&self, key: &str) -> Result<(), CacheError>;

    /// Atomically increment the value at `key` by `delta`, returning the new value.
    /// Backends that cannot increment return an error.
    async fn increment(&self, _key: &str, _delta: i64) -> Result<i64, CacheError> {
        Err(CacheError::Other(
            "increment not supported by this backend".into(),
        ))
    }

    /// Returns the remaining time-to-live of `key`, or `None` if the key does not exist.
    async fn ttl(&self, _key: &str) -> Result<Option<Duration>, CacheError> {
        Err(CacheError::Other(
            "ttl not supported by this backend".into(),
        ))
    }

    /// Fetch multiple keys in one round trip. Missing keys yield `None`.
    async fn multi_get(&self, _keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>, CacheError> {
        Err(CacheError::Other(
            "multi_get not supported by this backend".into(),
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache error: {0}")]
    Other(String),
}

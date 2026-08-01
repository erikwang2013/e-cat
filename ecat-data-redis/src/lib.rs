// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{Cache, CacheError};
use ecat_tls::TlsClientConfig;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    pub url: String,
    #[serde(default)]
    pub password: Option<String>,
    /// TLS configuration. When enabled, uses `rediss://` scheme.
    /// Cert paths are for future TLS connection parameter support.
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct RedisCache {
    conn: MultiplexedConnection,
}

impl RedisCache {
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let client =
            redis::Client::open(url).map_err(|e| CacheError::Other(format!("redis open: {e}")))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| CacheError::Other(format!("redis connect: {e}")))?;
        Ok(Self { conn })
    }

    pub async fn connect_with_password(
        url: &str,
        password: &str,
    ) -> Result<Self, CacheError> {
        let url = if url.contains('@') {
            url.to_string()
        } else {
            url.replacen("://", &format!("://:{password}@"), 1)
        };
        Self::connect(&url).await
    }

    pub async fn from_config(cfg: RedisConfig) -> Result<Self, CacheError> {
        let url = if cfg.tls.as_ref().is_some_and(|t| t.is_enabled()) {
            cfg.url.replacen("redis://", "rediss://", 1)
        } else {
            cfg.url
        };
        match &cfg.password {
            Some(pw) if !pw.is_empty() => Self::connect_with_password(&url, pw).await,
            _ => Self::connect(&url).await,
        }
    }

    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        let mut conn = self.conn.clone();
        conn.get(key)
            .await
            .map_err(|e| CacheError::Other(format!("redis get: {e}")))
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), CacheError> {
        let mut conn = self.conn.clone();
        let secs = ttl.as_secs();
        if secs > 0 {
            let (): () = conn
                .set_ex(key, value, secs)
                .await
                .map_err(|e| CacheError::Other(format!("redis setex: {e}")))?;
        } else {
            let (): () = conn
                .set(key, value)
                .await
                .map_err(|e| CacheError::Other(format!("redis set: {e}")))?;
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        let mut conn = self.conn.clone();
        conn.del(key)
            .await
            .map_err(|e| CacheError::Other(format!("redis del: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_fails_bad_url() {
        let result = RedisCache::connect("redis://nonexistent:9999").await;
        assert!(result.is_err());
    }
}

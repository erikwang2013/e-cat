// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{Cache, CacheError};
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use std::time::Duration;

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

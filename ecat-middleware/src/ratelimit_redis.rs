// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::ratelimit::RateLimitStore;
use async_trait::async_trait;
use redis::aio::MultiplexedConnection;

/// Redis-backed fixed-window rate limit store (keys prefixed with `rl:`).
pub struct RedisRateLimitStore {
    conn: MultiplexedConnection,
}

impl RedisRateLimitStore {
    pub async fn connect(url: &str) -> Result<Self, String> {
        let client = redis::Client::open(url).map_err(|e| e.to_string())?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl RateLimitStore for RedisRateLimitStore {
    async fn check(&self, key: &str, max: u32, window_secs: u64) -> Result<(), String> {
        let mut conn = self.conn.clone();
        let rkey = format!("rl:{key}");
        let count: i64 = redis::cmd("INCR")
            .arg(&rkey)
            .query_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        if count == 1 {
            let _: () = redis::cmd("EXPIRE")
                .arg(&rkey)
                .arg(window_secs)
                .query_async(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
        }
        if count as u32 > max {
            Err("rate limit exceeded".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_fails_bad_url() {
        let result = RedisRateLimitStore::connect("redis://nonexistent:9999").await;
        assert!(result.is_err());
    }
}

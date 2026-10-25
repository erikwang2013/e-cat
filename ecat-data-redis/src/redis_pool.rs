// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 共享 Redis 连接池：cache 与 lock 克隆同一个 `RedisPool`。
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;

use deadpool::Runtime;
use deadpool::managed::{Manager, Metrics, Object, Pool, RecycleError, RecycleResult};
use ecat_errors::{Error, ErrorCode};
use ecat_tls::TlsClientConfig;
use redis::ConnectionInfo;
use redis::aio::MultiplexedConnection;
use serde::Deserialize;

use super::{RedisCache, RedisLock, query_timeout};

const DEFAULT_MAX_SIZE: usize = 10;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisPoolConfig {
    pub url: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_size: Option<usize>,
    #[serde(default)]
    pub acquire_timeout_secs: Option<u64>,
    #[serde(default)]
    pub create_timeout_secs: Option<u64>,
    #[serde(default)]
    pub recycle_timeout_secs: Option<u64>,
}

pub(crate) struct RedisConnectionManager {
    client: redis::Client,
}

impl Manager for RedisConnectionManager {
    type Type = MultiplexedConnection;
    type Error = redis::RedisError;

    fn create(&self) -> impl Future<Output = Result<Self::Type, Self::Error>> + Send {
        let client = self.client.clone();
        async move { client.get_multiplexed_async_connection().await }
    }

    async fn recycle(
        &self,
        conn: &mut Self::Type,
        _metrics: &Metrics,
    ) -> RecycleResult<Self::Error> {
        let pong: String = redis::cmd("PING")
            .query_async(conn)
            .await
            .map_err(RecycleError::Backend)?;
        if pong.eq_ignore_ascii_case("PONG") {
            Ok(())
        } else {
            Err(RecycleError::message(
                "redis PING returned an unexpected response",
            ))
        }
    }
}

/// 内部 `Arc<Pool>` 让 `cache()` / `lock()` 共享同一组连接。
#[derive(Clone)]
pub struct RedisPool {
    pool: Arc<Pool<RedisConnectionManager>>,
    query_timeout: Option<Duration>,
}

impl RedisPool {
    /// 只建池，deadpool 惰性建连；首次取连接才发生 I/O。
    pub async fn connect(cfg: RedisPoolConfig) -> Result<Self, Error> {
        let url = build_pool_url(&cfg);
        let client = if let Some(password) = cfg.password.as_ref().filter(|p| !p.is_empty()) {
            let mut info: ConnectionInfo = url
                .parse()
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis url: {e}")))?;
            info.redis.password = Some(password.clone());
            redis::Client::open(info)
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis open: {e}")))?
        } else {
            redis::Client::open(url)
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis open: {e}")))?
        };
        let (max_size, acquire, create, recycle) = pool_defaults(&cfg);
        let pool = Pool::<RedisConnectionManager>::builder(RedisConnectionManager { client })
            .runtime(Runtime::Tokio1)
            .max_size(max_size)
            .wait_timeout(timeout_secs(acquire))
            .create_timeout(timeout_secs(create))
            .recycle_timeout(timeout_secs(recycle))
            .build()
            .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis pool: {e}")))?;
        Ok(Self {
            pool: Arc::new(pool),
            query_timeout: query_timeout(cfg.query_timeout_secs),
        })
    }

    pub fn cache(&self) -> RedisCache {
        RedisCache::from_pool(self.clone())
    }

    pub fn lock(&self) -> RedisLock {
        RedisLock::from_pool(self.clone())
    }

    pub fn status(&self) -> deadpool::Status {
        self.pool.status()
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Arc<Pool<RedisConnectionManager>> {
        &self.pool
    }

    pub(crate) fn query_timeout(&self) -> Option<Duration> {
        self.query_timeout
    }
}

fn build_pool_url(cfg: &RedisPoolConfig) -> String {
    if cfg.tls.as_ref().is_some_and(|t| t.is_enabled()) {
        cfg.url.replacen("redis://", "rediss://", 1)
    } else {
        cfg.url.clone()
    }
}

/// 返回 (max_size, acquire, create, recycle) 秒数；未配置用默认值。
fn pool_defaults(cfg: &RedisPoolConfig) -> (usize, u64, u64, u64) {
    (
        cfg.max_size.unwrap_or(DEFAULT_MAX_SIZE).max(1),
        cfg.acquire_timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        cfg.create_timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        cfg.recycle_timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
    )
}

/// 与 `query_timeout_secs` 一致：0 = 禁用 deadpool 超时。
fn timeout_secs(secs: u64) -> Option<Duration> {
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

pub(crate) enum RedisConnGuard {
    Single(MultiplexedConnection),
    Pooled(Object<RedisConnectionManager>),
}

impl Deref for RedisConnGuard {
    type Target = MultiplexedConnection;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Single(conn) => conn,
            Self::Pooled(conn) => conn,
        }
    }
}

impl DerefMut for RedisConnGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Single(conn) => conn,
            Self::Pooled(conn) => conn,
        }
    }
}

/// 命令路径统一走这里：Single 克隆多路复用连接，Pooled 取池连接 guard。
pub(crate) enum RedisBackend {
    Single(MultiplexedConnection),
    Pooled(RedisPool),
}

impl RedisBackend {
    pub(crate) async fn conn(&self) -> Result<RedisConnGuard, String> {
        match self {
            Self::Single(conn) => Ok(RedisConnGuard::Single(conn.clone())),
            Self::Pooled(pool) => pool
                .pool
                .get()
                .await
                .map(RedisConnGuard::Pooled)
                .map_err(|e| e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests;

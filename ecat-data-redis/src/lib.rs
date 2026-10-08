// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Redis 缓存客户端（`Cache` trait 实现）。
//!
//! # 能力边界
//!
//! 本 client 用 `MultiplexedConnection`（一条 TCP 服务所有并发），**不是连接池** ——
//! 对缓存负载这比池更优：连接数与往返都更低。代价是**有状态命令序列不能用它**：
//! `MULTI`/`EXEC` 事务、`WATCH`、`SUBSCRIBE`、`BLOCKING` 命令需要独占连接，
//! 多路复用下会与其它命令交错。需要时用 `redis::Client::get_async_connection()`
//! 另开一条专用连接。
//!
//! `Cache` 六个方法的**出站调用**都经过 `guarded`（熔断在外、超时在内，见批次 5a「出入 4」）；
//! 纯本地分支不走外壳 —— `multi_get` 空 keys 的提前返回就是如此，理由见
//! `RedisCache::guarded` 的注释。[`RedisLock`] 不在出站韧性范围内。
use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::Cache;
use ecat_data::breaker_error_to_backend_error;
use ecat_data::{BackendKind, run_with_timeout};
use ecat_errors::{Error, ErrorCode};
use ecat_lock::{DistributedLock, LockError};
use ecat_tls::TlsClientConfig;
use redis::AsyncCommands;
use redis::ConnectionInfo;
use redis::aio::MultiplexedConnection;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;

#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    pub url: String,
    #[serde(default)]
    pub password: Option<String>,
    /// TLS configuration. When enabled, uses `rediss://` scheme.
    /// Cert paths are for future TLS connection parameter support.
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次命令超时秒数。`0` = 禁用；未配置 = 30 秒。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    ///
    /// 熔断**默认开启** —— 保守阈值下只在持续失败时打开。**当前没有总开关**：
    /// `BreakerConfig` 只有阈值字段，没有 `enabled`（计划 Task 7 也点名不许写
    /// `{"enabled": false}`：那是反序列化错误，或被 `#[serde(default)]` 静默吞掉
    /// 后以为关掉了）。真要停用，只能把阈值调到不可能触发（如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
}

/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

fn build_url(cfg: &RedisConfig) -> String {
    if cfg.tls.as_ref().is_some_and(|t| t.is_enabled()) {
        cfg.url.replacen("redis://", "rediss://", 1)
    } else {
        cfg.url.clone()
    }
}

pub struct RedisCache {
    conn: MultiplexedConnection,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
}

impl RedisCache {
    pub async fn connect(url: &str) -> Result<Self, Error> {
        let client = redis::Client::open(url)
            .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis open: {e}")))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis connect: {e}")))?;
        Ok(Self {
            conn,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
        })
    }

    pub async fn connect_with_password(url: &str, password: &str) -> Result<Self, Error> {
        // 通过 ConnectionInfo 单独传密码，避免密码嵌入 URL（否则错误消息会泄露口令）
        let mut info: ConnectionInfo = url
            .parse()
            .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis url: {e}")))?;
        info.redis.password = Some(password.to_string());
        let client = redis::Client::open(info)
            .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis open: {e}")))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis connect: {e}")))?;
        Ok(Self {
            conn,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
        })
    }

    // Reconnection behavior: there is no explicit reconnect logic here.
    // The underlying redis::aio::MultiplexedConnection reconnects
    // internally on transient failures; a dropped connection is detected
    // on the next command, which will return an error.
    pub async fn from_config(cfg: RedisConfig) -> Result<Self, Error> {
        let url = build_url(&cfg);
        let client = match &cfg.password {
            Some(pw) if !pw.is_empty() => Self::connect_with_password(&url, pw).await?,
            _ => Self::connect(&url).await?,
        };
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // 自动接线（lead 裁决 2026-10-08）：`metrics` feature 下**构造即注册**，
        // 用户代码零变化。`ecat_metrics::register_outbound_metrics` 是幂等的
        // （同一 backend 重复注册是覆盖闭包），所以多 client 不会炸。
        // 探针：注释掉下面两行 ⇒ from_config_registers_outbound_metrics 红。
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            conn: client.conn,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
        })
    }

    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self {
            conn,
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
        }
    }

    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 一次出站调用的公共外壳：**熔断在外、超时在内**。
    ///
    /// 顺序与 spec §3 相反（理由见批次 5a 计划的「与 spec 的出入 4」）：
    /// 超时若在外层，`tokio::time::timeout` 会把熔断器的 future 直接 drop 掉，
    /// 于是每次「后端没在预算内作答」都**什么都不记** —— 卡死的后端永远打不开熔断器，
    /// 而卡死正是本设计要防的头号场景。熔断在外时超时是一次普通的 `Err`，如实计入失败。
    ///
    /// **只包真正发 I/O 的路径。** 纯本地分支（如 `multi_get` 的空 keys 提前返回）
    /// 必须留在外面：半开态下每次 `call` 都要借走一个探测名额，而「没发请求就返回」
    /// 会以 `Ok` 记账 —— 探测名额被白白消耗、熔断器还可能被**误关回 `Closed`**，
    /// 于是新窗口的流量全部冲向一个根本没被碰过的后端。记账口径是「后端的表现」，
    /// 不是「函数的返回值」。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        self.breaker
            .call(|| run_with_timeout(BackendKind::Cache, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "redis"))
    }
}

#[async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        self.guarded(async {
            let mut conn = self.conn.clone();
            conn.get(key)
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis get: {e}")))
        })
        .await
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), Error> {
        self.guarded(async {
            let mut conn = self.conn.clone();
            let millis = ttl.as_millis();
            if millis > 0 {
                let ms = if millis > u64::MAX as u128 {
                    u64::MAX
                } else {
                    millis as u64
                };
                let (): () = conn.pset_ex(key, value, ms).await.map_err(|e| {
                    Error::new(ErrorCode::Internal, "redis", format!("redis psetex: {e}"))
                })?;
            } else {
                let (): () = conn.set(key, value).await.map_err(|e| {
                    Error::new(ErrorCode::Internal, "redis", format!("redis set: {e}"))
                })?;
            }
            Ok(())
        })
        .await
    }

    async fn delete(&self, key: &str) -> Result<(), Error> {
        self.guarded(async {
            let mut conn = self.conn.clone();
            conn.del(key)
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis del: {e}")))
        })
        .await
    }

    async fn increment(&self, key: &str, delta: i64) -> Result<i64, Error> {
        self.guarded(async {
            let mut conn = self.conn.clone();
            conn.incr(key, delta)
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis incr: {e}")))
        })
        .await
    }

    async fn ttl(&self, key: &str) -> Result<Option<Duration>, Error> {
        self.guarded(async {
            let mut conn = self.conn.clone();
            let ttl: i64 = conn
                .ttl(key)
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis ttl: {e}")))?;
            Ok(ttl_to_duration(ttl))
        })
        .await
    }

    async fn multi_get(&self, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>, Error> {
        // 没发 I/O，就不经过外壳。半开态下走 `guarded` 会借一个探测名额、再以 `Ok`
        // 记一笔 —— 熔断器可能据此关回 `Closed`，而这次「成功」根本没碰过后端。
        // 详见 `guarded` 的注释。
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        self.guarded(async {
            let mut conn = self.conn.clone();
            conn.mget(keys)
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis mget: {e}")))
        })
        .await
    }
}

/// redis TTL 语义映射：-2 表示键不存在、-1 表示无过期时间，均映射为 None；
/// 其余为剩余秒数。
fn ttl_to_duration(ttl: i64) -> Option<Duration> {
    if ttl < 0 {
        None
    } else {
        Some(Duration::from_secs(ttl as u64))
    }
}

/// Distributed lock backed by Redis `SET NX PX`.
pub struct RedisLock {
    conn: MultiplexedConnection,
}

impl RedisLock {
    pub async fn connect(url: &str) -> Result<Self, LockError> {
        let client =
            redis::Client::open(url).map_err(|e| LockError::Other(format!("redis open: {e}")))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| LockError::Other(format!("redis connect: {e}")))?;
        Ok(Self { conn })
    }

    pub async fn from_config(cfg: RedisConfig) -> Result<Self, LockError> {
        let url = build_url(&cfg);
        if let Some(pw) = cfg.password.as_ref().filter(|p| !p.is_empty()) {
            // 通过 ConnectionInfo 单独传密码，避免密码嵌入 URL 后泄露在错误消息中
            let mut info: ConnectionInfo = url
                .parse()
                .map_err(|e| LockError::Other(format!("redis url: {e}")))?;
            info.redis.password = Some(pw.clone());
            let client = redis::Client::open(info)
                .map_err(|e| LockError::Other(format!("redis open: {e}")))?;
            let conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| LockError::Other(format!("redis connect: {e}")))?;
            Ok(Self { conn })
        } else {
            Self::connect(&url).await
        }
    }

    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl DistributedLock for RedisLock {
    async fn acquire(&self, key: &str, ttl: Duration) -> Result<Option<String>, LockError> {
        let mut conn = self.conn.clone();
        let token = Uuid::new_v4().to_string();
        let millis = ttl.as_millis();
        // 与 Cache::set 保持一致：ttl 溢出时钳制为 u64::MAX
        let px = if millis > u64::MAX as u128 {
            u64::MAX
        } else {
            millis as u64
        };
        let acquired: Option<()> = conn
            .set_options(
                key,
                token.as_str(),
                redis::SetOptions::default()
                    .conditional_set(redis::ExistenceCheck::NX)
                    .with_expiration(redis::SetExpiry::PX(px)),
            )
            .await
            .map_err(|e| LockError::Other(format!("redis acquire: {e}")))?;
        if acquired.is_some() {
            Ok(Some(token))
        } else {
            Ok(None)
        }
    }

    async fn release(&self, key: &str, token: &str) -> Result<(), LockError> {
        let mut conn = self.conn.clone();
        // Compare-and-delete: only release when the token still matches the holder.
        let script = r#"
            if redis.call("get", KEYS[1]) == ARGV[1] then
                return redis.call("del", KEYS[1])
            else
                return 0
            end
        "#;
        let (): () = redis::Script::new(script)
            .key(key)
            .arg(token)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| LockError::Other(format!("redis release: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 共享池测试：默认值、共享 Arc、旧构造器兼容；live 用例按 env 门控。
use std::sync::Arc;
use std::time::Duration;

use ecat_data::Cache;
use ecat_lock::DistributedLock;

use super::{RedisPool, RedisPoolConfig, pool_defaults};
use crate::{RedisCache, RedisConfig, RedisLock};

fn config(url: &str) -> RedisPoolConfig {
    RedisPoolConfig {
        url: url.to_owned(),
        password: None,
        tls: None,
        query_timeout_secs: None,
        max_size: None,
        acquire_timeout_secs: None,
        create_timeout_secs: None,
        recycle_timeout_secs: None,
    }
}

#[test]
fn pool_config_defaults_are_sane() {
    let cfg: RedisPoolConfig = serde_json::from_str(r#"{"url":"redis://localhost:6379"}"#).unwrap();
    assert_eq!(pool_defaults(&cfg), (10, 30, 30, 30));
    assert!(cfg.max_size.is_none());
    assert!(cfg.acquire_timeout_secs.is_none());
    assert!(cfg.create_timeout_secs.is_none());
    assert!(cfg.recycle_timeout_secs.is_none());
}

#[tokio::test]
async fn from_pool_config_rejects_bad_url() {
    assert!(RedisPool::connect(config("not-a-url")).await.is_err());
}

#[tokio::test]
async fn cache_and_lock_share_one_pool() {
    let pool = RedisPool::connect(config("redis://127.0.0.1:1"))
        .await
        .expect("pool construction is lazy");
    let cache = pool.cache();
    let lock = pool.lock();

    let cache_pool = cache.pool_for_test().expect("cache must be pooled");
    let lock_pool = lock.pool_for_test().expect("lock must be pooled");
    assert!(Arc::ptr_eq(pool.inner(), cache_pool.inner()));
    assert!(Arc::ptr_eq(cache_pool.inner(), lock_pool.inner()));
}

#[test]
fn old_connect_and_from_config_still_compile() {
    // 只构造 future，不 poll：旧签名可编译且不会发起 I/O。
    drop(RedisCache::connect("redis://127.0.0.1:1"));
    drop(RedisLock::connect("redis://127.0.0.1:1"));
    let cfg = RedisConfig {
        url: "redis://127.0.0.1:1".into(),
        password: None,
        tls: None,
        query_timeout_secs: None,
        breaker: None,
    };
    drop(RedisCache::from_config(cfg.clone()));
    drop(RedisLock::from_config(cfg));
}

#[tokio::test]
async fn pool_status_reports_size() {
    let mut cfg = config("redis://127.0.0.1:1");
    cfg.max_size = Some(7);
    let pool = RedisPool::connect(cfg).await.expect("lazy pool");
    let status = pool.status();
    assert_eq!(status.max_size, 7);
    assert_eq!(status.size, 0, "lazy pool must not connect on construction");
}

#[tokio::test]
async fn pool_handles_concurrent_cache_and_lock() {
    let Ok(url) = std::env::var("ECAT_TEST_REDIS_URL") else {
        eprintln!("skip pool_handles_concurrent_cache_and_lock: ECAT_TEST_REDIS_URL not set");
        return;
    };
    let mut cfg = config(&url);
    cfg.max_size = Some(4);
    let pool = RedisPool::connect(cfg).await.expect("pool");
    let cache = pool.cache();
    let lock = pool.lock();
    let key = format!("ecat:test:pool:{}", uuid::Uuid::new_v4());

    let (set, acquired) = tokio::join!(
        cache.set(&key, b"v", Duration::from_secs(30)),
        lock.acquire(&key, Duration::from_secs(30))
    );
    set.expect("cache set");
    let token = acquired
        .expect("lock acquire")
        .expect("fresh lock key must be acquirable");
    assert_eq!(
        cache.get(&key).await.unwrap(),
        Some(b"v".to_vec()),
        "cache read must see the concurrent write"
    );
    lock.release(&key, &token).await.expect("release");
    cache.delete(&key).await.expect("cleanup");
    assert!(pool.status().size >= 1, "pool must have served connections");
}

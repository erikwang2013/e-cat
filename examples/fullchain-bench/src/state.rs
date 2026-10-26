// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 全链路基准的连接、种子数据与读写辅助函数。
use std::sync::Arc;
use std::time::Duration;

use ecat_data::{Cache, RdbmsClient, SqlExecutor};
use ecat_data_redis::{RedisCache, RedisConfig};
use ecat_data_sqlx::{PoolParams, SqlxClient};
use serde_json::{Value, json};

pub const ROWS: i64 = 10_000;
pub const TABLE: &str = "bench_kv";

#[derive(Clone)]
pub struct BenchState {
    pub db: Arc<SqlxClient>,
    pub redis: Arc<RedisCache>,
}

pub fn mysql_url() -> String {
    std::env::var("ECAT_BENCH_MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:root@127.0.0.1:3307/ecat?ssl-mode=DISABLED".into())
}

pub fn redis_url() -> String {
    std::env::var("ECAT_BENCH_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into())
}

pub async fn connect_db(url: &str) -> Result<Arc<SqlxClient>, String> {
    let mut params = PoolParams::for_url(url);
    // 8 并发 + 余量；预热到 16 条，避免首个请求承担建连成本。
    params.max_connections = 16;
    params.min_connections = 16;
    // 启动时一次建满 16 条连接；100ms 在负载高的笔记本上会让 warm_up 自身超时。
    params.acquire_timeout = Duration::from_secs(2);
    // 基准要观察真实尾部；30s 的 tokio timer 会污染每次查询的测量。
    params.query_timeout = None;
    params.slow_query = None;
    params.test_before_acquire = false;
    let client = SqlxClient::connect_with_params(url, &params)
        .await
        .map_err(|error| format!("mysql connect: {error}"))?;
    client
        .warm_up()
        .await
        .map_err(|error| format!("mysql warm_up: {error}"))?;
    Ok(Arc::new(client))
}

pub async fn connect_redis(url: &str) -> Result<Arc<RedisCache>, String> {
    let config = RedisConfig {
        url: url.to_owned(),
        password: None,
        tls: None,
        query_timeout_secs: Some(0),
        breaker: None,
    };
    RedisCache::from_config(config)
        .await
        .map(Arc::new)
        .map_err(|error| format!("redis connect: {error}"))
}

pub async fn connect_state() -> Result<BenchState, String> {
    let db = connect_db(&mysql_url()).await?;
    let redis = connect_redis(&redis_url()).await?;
    Ok(BenchState { db, redis })
}

/// 建表并补齐 10k 行；重复运行只补缺失行，Redis key 每次重写保证热数据。
pub async fn seed(state: &BenchState) -> Result<(), String> {
    state
        .db
        .execute(&format!(
            "CREATE TABLE IF NOT EXISTS {TABLE} \
             (id BIGINT PRIMARY KEY, v VARCHAR(64) NOT NULL) ENGINE=InnoDB"
        ))
        .await
        .map_err(|error| format!("create table: {error}"))?;

    // 逐 id upsert：不能拿 COUNT(*) 当“下一个 id”，否则删除中间行或 id 偏移时
    // 会漏种/撞主键。
    let tx = state
        .db
        .transaction()
        .await
        .map_err(|error| format!("seed transaction: {error}"))?;
    for id in 0..ROWS {
        let value = value_for(id);
        tx.execute_with(
            &format!("INSERT INTO {TABLE} (id, v) VALUES (?, ?) ON DUPLICATE KEY UPDATE v = ?"),
            &[json!(id), json!(value), json!(value)],
        )
        .await
        .map_err(|error| format!("seed row {id}: {error}"))?;
    }
    tx.commit()
        .await
        .map_err(|error| format!("seed commit: {error}"))?;

    for id in 0..ROWS {
        state
            .redis
            .set(
                &cache_key(id),
                value_for(id).as_bytes(),
                Duration::from_secs(3600),
            )
            .await
            .map_err(|error| format!("seed cache {id}: {error}"))?;
    }
    Ok(())
}

pub async fn read(state: &BenchState, id: i64) -> Result<(String, usize), String> {
    match tokio::time::timeout(Duration::from_secs(2), read_inner(state, id)).await {
        Ok(result) => result,
        Err(_) => Err(format!("read timed out for id {id}")),
    }
}

async fn read_inner(state: &BenchState, id: i64) -> Result<(String, usize), String> {
    let rows = state
        .db
        .query_with(&format!("SELECT v FROM {TABLE} WHERE id = ?"), &[json!(id)])
        .await
        .map_err(|error| format!("mysql read: {error}"))?;
    let value = rows
        .first()
        .and_then(|row| row.get("v"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("mysql read: row {id} missing"))?
        .to_owned();
    let cached = state
        .redis
        .get(&cache_key(id))
        .await
        .map_err(|error| format!("redis get: {error}"))?;
    let cache_len = cached.as_ref().map_or(0, Vec::len);
    if cache_len == 0 {
        return Err(format!("redis get: key {id} missing"));
    }
    Ok((value, cache_len))
}

pub async fn write(state: &BenchState, id: i64, value: &str) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_secs(2), write_inner(state, id, value)).await {
        Ok(result) => result,
        Err(_) => Err(format!("write timed out for id {id}")),
    }
}

async fn write_inner(state: &BenchState, id: i64, value: &str) -> Result<(), String> {
    state
        .db
        .execute_with(
            &format!("UPDATE {TABLE} SET v = ? WHERE id = ?"),
            &[json!(value), json!(id)],
        )
        .await
        .map_err(|error| format!("mysql write: {error}"))?;
    state
        .redis
        .set(&cache_key(id), value.as_bytes(), Duration::from_secs(3600))
        .await
        .map_err(|error| format!("redis set: {error}"))?;
    Ok(())
}

pub fn cache_key(id: i64) -> String {
    format!("bench:{id}")
}

pub fn value_for(id: i64) -> String {
    format!("value-{id}")
}

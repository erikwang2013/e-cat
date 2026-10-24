// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! health 测试：拓扑、分片连通性、超时与 outbox 积压；全部用假客户端，不连真库。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use ecat_health::HealthCheck;
use serde_json::Value;

use super::{OutboxHealthCheck, ShardHealthCheck};
use crate::{OutboxStore, ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardRouter, ShardTable};

fn catalog(shards: &[&str]) -> ShardCatalog {
    ShardCatalog {
        tables: vec![ShardTable {
            name: "users".into(),
            key: "id".into(),
            pk: "id".into(),
            algorithm: ShardAlgorithm::HashMod {
                shards: shards.len() as u32,
            },
            shards: shards
                .iter()
                .map(|name| ShardEndpoint {
                    name: (*name).into(),
                    url: None,
                })
                .collect(),
        }],
    }
}

/// 假客户端：`fail` 模拟分片宕机，`delay` 模拟慢分片，`rows` 供 outbox 计数。
struct FakeClient {
    fail: bool,
    delay: Duration,
    rows: Vec<Row>,
    sql: Mutex<Vec<String>>,
}

impl FakeClient {
    fn ok() -> Self {
        Self {
            fail: false,
            delay: Duration::ZERO,
            rows: vec![Row::new(vec!["1".into()], vec![Value::from(1)])],
            sql: Mutex::new(Vec::new()),
        }
    }

    fn down() -> Self {
        Self {
            fail: true,
            ..Self::ok()
        }
    }

    fn slow(delay: Duration) -> Self {
        Self {
            delay,
            ..Self::ok()
        }
    }

    fn count(n: u64) -> Self {
        Self {
            rows: vec![Row::new(vec!["n".into()], vec![Value::from(n)])],
            ..Self::ok()
        }
    }

    fn sent(&self) -> Vec<String> {
        self.sql.lock().unwrap().clone()
    }
}

#[async_trait]
impl SqlExecutor for FakeClient {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database("health probes only use query".into()))
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.sql.lock().unwrap().push(sql.to_owned());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if self.fail {
            Err(RdbmsError::Connection("shard down".into()))
        } else {
            Ok(self.rows.clone())
        }
    }

    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }
}

#[async_trait]
impl RdbmsClient for FakeClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Ok(Transaction::new())
    }
}

fn router(clients: Vec<(&str, Arc<FakeClient>)>) -> Arc<ShardRouter> {
    let names: Vec<&str> = clients.iter().map(|(name, _)| *name).collect();
    let clients = clients
        .into_iter()
        .map(|(name, client)| (name.to_owned(), client as Arc<dyn RdbmsClient>))
        .collect();
    Arc::new(ShardRouter::new(catalog(&names), clients).expect("valid topology"))
}

#[tokio::test]
async fn shard_health_ok_when_all_shards_answer() {
    let s0 = Arc::new(FakeClient::ok());
    let s1 = Arc::new(FakeClient::ok());
    let router = router(vec![("s0", Arc::clone(&s0)), ("s1", Arc::clone(&s1))]);
    let check = ShardHealthCheck::new(router, Duration::from_secs(1));

    assert_eq!(check.name(), "data.shards");
    assert_eq!(check.check().await, Ok(()));
    assert_eq!(s0.sent(), vec!["SELECT 1"]);
    assert_eq!(s1.sent(), vec!["SELECT 1"]);
}

#[tokio::test]
async fn shard_health_fails_when_one_shard_is_down() {
    let router = router(vec![
        ("s0", Arc::new(FakeClient::ok())),
        ("s1", Arc::new(FakeClient::down())),
    ]);
    let check = ShardHealthCheck::new(router, Duration::from_secs(1));

    let err = check.check().await.unwrap_err();
    assert!(err.contains("s1"), "error names the failed shard: {err}");
}

#[tokio::test]
async fn shard_health_fails_on_invalid_topology() {
    let mut bad = catalog(&["s0", "s1"]);
    bad.tables[0].algorithm = ShardAlgorithm::HashMod { shards: 3 };
    let router = Arc::new(ShardRouter {
        catalog: bad,
        clients: HashMap::new(),
    });
    let check = ShardHealthCheck::new(router, Duration::from_secs(1));

    let err = check.check().await.unwrap_err();
    assert!(err.contains("shard count mismatch"), "{err}");
}

#[tokio::test]
async fn shard_health_fails_when_probe_times_out() {
    let router = router(vec![(
        "s0",
        Arc::new(FakeClient::slow(Duration::from_secs(30))),
    )]);
    let check = ShardHealthCheck::new(router, Duration::from_millis(20));

    let err = check.check().await.unwrap_err();
    assert!(err.contains("timed out"), "{err}");
}

#[tokio::test]
async fn outbox_health_fails_over_backlog() {
    let check = OutboxHealthCheck::new(
        Arc::new(OutboxStore::new(Arc::new(FakeClient::count(6)))),
        5,
    );

    assert_eq!(check.name(), "data.outbox");
    let err = check.check().await.unwrap_err();
    assert!(err.contains('6') && err.contains('5'), "{err}");

    // 等于阈值不算超。
    let at_limit = OutboxHealthCheck::new(
        Arc::new(OutboxStore::new(Arc::new(FakeClient::count(5)))),
        5,
    );
    assert_eq!(at_limit.check().await, Ok(()));
}

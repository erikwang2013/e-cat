// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 双写路由/回滚语义：物理同一性只认客户端 `Arc`，客户端不同时默认要求显式策略；
//! 镜像失败只能通过 `mirror_client()` 重放，不能整体重试。
#![cfg(feature = "migrate")]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use ecat_data_sharding::{
    DualWrite, ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardRouter, ShardTable,
};
use ecat_data_sqlx::{PoolParams, SqlxClient};
use serde_json::{Value, json};

const INCREMENT: &str = "UPDATE orders SET amount = amount + 1 WHERE id = ?";

async fn db() -> Arc<dyn RdbmsClient> {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    Arc::new(
        SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap(),
    )
}

async fn create_orders(db: &Arc<dyn RdbmsClient>) {
    db.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER)")
        .await
        .unwrap();
}

async fn insert_amount(db: &Arc<dyn RdbmsClient>, id: i64, amount: i64) {
    db.execute_with(
        "INSERT INTO orders (id, amount) VALUES (?, ?)",
        &[json!(id), json!(amount)],
    )
    .await
    .unwrap();
}

async fn amount(db: &Arc<dyn RdbmsClient>, id: i64) -> i64 {
    db.query_with("SELECT amount FROM orders WHERE id = ?", &[json!(id)])
        .await
        .unwrap()[0]
        .get("amount")
        .unwrap()
        .as_i64()
        .unwrap()
}

fn table(algorithm: ShardAlgorithm, shards: &[&str]) -> ShardTable {
    ShardTable {
        name: "orders".into(),
        key: "id".into(),
        pk: "id".into(),
        algorithm,
        shards: shards
            .iter()
            .map(|name| ShardEndpoint {
                name: (*name).into(),
                url: None,
            })
            .collect(),
    }
}

fn hash_catalog(shards: &[&str]) -> ShardCatalog {
    ShardCatalog {
        tables: vec![table(
            ShardAlgorithm::HashMod {
                shards: shards.len() as u32,
            },
            shards,
        )],
    }
}

fn client_map(entries: &[(&str, &Arc<dyn RdbmsClient>)]) -> HashMap<String, Arc<dyn RdbmsClient>> {
    entries
        .iter()
        .map(|(name, client)| ((*name).to_owned(), Arc::clone(client)))
        .collect()
}

fn one_shard_catalog(name: &str) -> ShardCatalog {
    ShardCatalog {
        tables: vec![ShardTable {
            name: "orders".into(),
            key: "id".into(),
            pk: "id".into(),
            algorithm: ShardAlgorithm::HashMod { shards: 1 },
            shards: vec![ShardEndpoint {
                name: name.into(),
                url: None,
            }],
        }],
    }
}

fn router_for(catalog: &ShardCatalog, shard: &str, db: &Arc<dyn RdbmsClient>) -> ShardRouter {
    ShardRouter::new(catalog.clone(), client_map(&[(shard, db)])).unwrap()
}

/// 第一次 `execute_with` 失败的镜像；用于验证 `mirror_client()` 重放路径。
struct FailOnce {
    inner: Arc<dyn RdbmsClient>,
    failed: AtomicBool,
}

impl FailOnce {
    fn new(inner: Arc<dyn RdbmsClient>) -> Self {
        Self {
            inner,
            failed: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl SqlExecutor for FailOnce {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.inner.execute(sql).await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.inner.query(sql).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<u64, RdbmsError> {
        if !self.failed.swap(true, Ordering::SeqCst) {
            return Err(RdbmsError::Database("transient mirror failure".into()));
        }
        self.inner.execute_with(sql, params).await
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.inner.query_with(sql, params).await
    }

    async fn query_write(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.inner.query_write(sql, params).await
    }

    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.inner
            .execute_then_query(first, first_params, second)
            .await
    }

    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }
}

#[async_trait]
impl RdbmsClient for FailOnce {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.inner.transaction().await
    }
}

struct DualFixture {
    s0: Arc<dyn RdbmsClient>,
    s1: Arc<dyn RdbmsClient>,
    s2: Arc<dyn RdbmsClient>,
    old_router: ShardRouter,
    new_router: ShardRouter,
    old: ShardCatalog,
    new: ShardCatalog,
}

async fn dual_fixture() -> DualFixture {
    let s0 = db().await;
    let s1 = db().await;
    let s2 = db().await;
    for shard in [&s0, &s1, &s2] {
        create_orders(shard).await;
    }
    let old = hash_catalog(&["s0", "s1"]);
    let new = hash_catalog(&["s0", "s1", "s2"]);
    let old_router =
        ShardRouter::new(old.clone(), client_map(&[("s0", &s0), ("s1", &s1)])).unwrap();
    let new_router = ShardRouter::new(
        new.clone(),
        client_map(&[("s0", &s0), ("s1", &s1), ("s2", &s2)]),
    )
    .unwrap();
    DualFixture {
        s0,
        s1,
        s2,
        old_router,
        new_router,
        old,
        new,
    }
}

/// 找一个在旧拓扑落 `old_shard`、在新拓扑落 `new_shard` 的键。
fn key_for(old: &ShardCatalog, new: &ShardCatalog, old_shard: usize, new_shard: usize) -> i64 {
    (1..100_000)
        .find(|id| {
            old.shard_for("orders", &json!(id)).unwrap() == old_shard
                && new.shard_for("orders", &json!(id)).unwrap() == new_shard
        })
        .expect("no key maps to the requested shard pair")
}

#[tokio::test]
async fn dual_write_before_cutover_reaches_old_and_new() {
    let f = dual_fixture().await;
    let key = key_for(&f.old, &f.new, 1, 2);
    insert_amount(&f.s1, key, 10).await;
    insert_amount(&f.s2, key, 100).await;

    let guard = DualWrite::before_cutover(&f.old_router, &f.new_router).assume_distinct();
    let client = guard.for_key("orders", &json!(key)).unwrap();
    client.execute_with(INCREMENT, &[json!(key)]).await.unwrap();

    assert_eq!(amount(&f.s1, key).await, 11, "old topology primary write");
    assert_eq!(amount(&f.s2, key).await, 101, "new topology mirror write");

    // 读走旧（主）拓扑。
    let rows = client
        .query_with("SELECT amount FROM orders WHERE id = ?", &[json!(key)])
        .await
        .unwrap();
    assert_eq!(rows[0].get("amount"), Some(&json!(11)));
}

#[tokio::test]
async fn dual_write_writes_unchanged_shard_once() {
    let f = dual_fixture().await;
    let key = key_for(&f.old, &f.new, 0, 0);
    insert_amount(&f.s0, key, 5).await;

    let client = DualWrite::before_cutover(&f.old_router, &f.new_router)
        .assume_distinct()
        .for_key("orders", &json!(key))
        .unwrap();
    client.execute_with(INCREMENT, &[json!(key)]).await.unwrap();

    assert_eq!(
        amount(&f.s0, key).await,
        6,
        "same client Arc must be written once"
    );
}

#[tokio::test]
async fn dual_write_after_cutover_keeps_rollback_topology_current() {
    let f = dual_fixture().await;
    let key = key_for(&f.old, &f.new, 1, 2);
    insert_amount(&f.s1, key, 10).await;
    insert_amount(&f.s2, key, 100).await;

    DualWrite::before_cutover(&f.old_router, &f.new_router)
        .assume_distinct()
        .for_key("orders", &json!(key))
        .unwrap()
        .execute_with(INCREMENT, &[json!(key)])
        .await
        .unwrap();

    let after = DualWrite::after_cutover(&f.new_router, &f.old_router).assume_distinct();
    let client = after.for_key("orders", &json!(key)).unwrap();
    client.execute_with(INCREMENT, &[json!(key)]).await.unwrap();

    assert_eq!(amount(&f.s2, key).await, 102, "new topology primary write");
    assert_eq!(
        amount(&f.s1, key).await,
        12,
        "old topology must stay current for rollback"
    );
    let rows = client
        .query_with("SELECT amount FROM orders WHERE id = ?", &[json!(key)])
        .await
        .unwrap();
    assert_eq!(rows[0].get("amount"), Some(&json!(102)));

    // rollback 后由旧拓扑服务：切流后的写已经镜像过来。
    let rolled_back = DualWrite::before_cutover(&f.old_router, &f.new_router).assume_distinct();
    let rows = rolled_back
        .for_key("orders", &json!(key))
        .unwrap()
        .query_with("SELECT amount FROM orders WHERE id = ?", &[json!(key)])
        .await
        .unwrap();
    assert_eq!(rows[0].get("amount"), Some(&json!(12)));
}

#[tokio::test]
async fn dual_write_mirror_failure_retry_via_mirror_client() {
    let old_db = db().await;
    create_orders(&old_db).await;
    insert_amount(&old_db, 1, 10).await;
    let new_db = db().await;
    create_orders(&new_db).await;
    insert_amount(&new_db, 1, 100).await;
    let flaky: Arc<dyn RdbmsClient> = Arc::new(FailOnce::new(Arc::clone(&new_db)));
    let old_router = router_for(&one_shard_catalog("old0"), "old0", &old_db);
    let new_router = router_for(&one_shard_catalog("new0"), "new0", &flaky);
    let client = DualWrite::before_cutover(&old_router, &new_router)
        .assume_distinct()
        .for_key("orders", &json!(1))
        .unwrap();

    let err = client
        .execute_with(INCREMENT, &[json!(1)])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("dual-write mirror failed"), "{err}");
    assert!(err.contains("mirror_client()"), "{err}");
    assert_eq!(
        amount(&old_db, 1).await,
        11,
        "primary committed exactly once"
    );
    assert_eq!(amount(&new_db, 1).await, 100, "mirror still behind");

    client
        .mirror_client()
        .expect("different clients have a mirror")
        .execute_with(INCREMENT, &[json!(1)])
        .await
        .unwrap();
    assert_eq!(
        amount(&old_db, 1).await,
        11,
        "replay must not touch primary"
    );
    assert_eq!(amount(&new_db, 1).await, 101, "mirror caught up");
}

#[tokio::test]
async fn dual_write_reports_mirror_failure_and_keeps_primary_write() {
    let f = dual_fixture().await;
    let key = key_for(&f.old, &f.new, 1, 2);
    insert_amount(&f.s1, key, 10).await;
    insert_amount(&f.s2, key, 100).await;
    f.s2.execute("DROP TABLE orders").await.unwrap();

    let client = DualWrite::before_cutover(&f.old_router, &f.new_router)
        .assume_distinct()
        .for_key("orders", &json!(key))
        .unwrap();
    let err = client
        .execute_with(INCREMENT, &[json!(key)])
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("dual-write mirror failed"), "{err}");
    assert_eq!(amount(&f.s1, key).await, 11);
}

#[tokio::test]
async fn dual_write_primary_failure_skips_mirror() {
    let f = dual_fixture().await;
    let key = key_for(&f.old, &f.new, 1, 2);
    insert_amount(&f.s1, key, 10).await;
    insert_amount(&f.s2, key, 100).await;
    f.s1.execute("DROP TABLE orders").await.unwrap();

    let client = DualWrite::before_cutover(&f.old_router, &f.new_router)
        .assume_distinct()
        .for_key("orders", &json!(key))
        .unwrap();
    let err = client
        .execute_with(INCREMENT, &[json!(key)])
        .await
        .unwrap_err()
        .to_string();

    assert!(!err.contains("dual-write mirror failed"), "{err}");
    assert_eq!(amount(&f.s2, key).await, 100, "mirror must stay untouched");
}

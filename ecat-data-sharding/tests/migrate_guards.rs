// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 迁移保护：二进制列 fail closed，以及 hash 重分片的双写路由/回滚语义。
#![cfg(feature = "migrate")]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use ecat_data_sharding::{
    DualWrite, KeyRange, ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardMigrator, ShardMove,
    ShardPlan, ShardRouter, ShardTable,
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

async fn row_count(db: &Arc<dyn RdbmsClient>) -> i64 {
    db.query("SELECT COUNT(*) AS n FROM orders").await.unwrap()[0]
        .get("n")
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

fn range_catalog(boundaries: &[i64], shards: &[&str]) -> ShardCatalog {
    ShardCatalog {
        tables: vec![table(
            ShardAlgorithm::Range {
                boundaries: boundaries.to_vec(),
            },
            shards,
        )],
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

fn move_s0_to_s1() -> ShardMove {
    ShardMove {
        table: "orders".into(),
        from: "s0".into(),
        to: "s1".into(),
        key_range: Some(KeyRange {
            low: Some(0),
            high: Some(100),
        }),
    }
}

fn boundaries(catalog: &ShardCatalog) -> Vec<i64> {
    match &catalog.tables[0].algorithm {
        ShardAlgorithm::Range { boundaries } => boundaries.clone(),
        other => panic!("expected range algorithm, got {other:?}"),
    }
}

// ---- 二进制列预检 ----

struct BinaryFixture {
    migrator: ShardMigrator,
    target: Arc<dyn RdbmsClient>,
    router: ShardRouter,
    new_catalog: ShardCatalog,
}

async fn binary_fixture() -> BinaryFixture {
    let source = db().await;
    source
        .execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER, payload BLOB)")
        .await
        .unwrap();
    source
        .execute("INSERT INTO orders (id, amount, payload) VALUES (1, 10, X'0102FF')")
        .await
        .unwrap();
    let target = db().await;
    target
        .execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER, payload BLOB)")
        .await
        .unwrap();
    let state = db().await;

    let old = range_catalog(&[100], &["s0", "s1"]);
    let new = range_catalog(&[0], &["s0", "s1"]);
    let plan = ShardPlan {
        moves: vec![move_s0_to_s1()],
    };
    let clients = client_map(&[("s0", &source), ("s1", &target)]);
    let migrator =
        ShardMigrator::new(plan, clients.clone(), state, old.clone(), new.clone()).unwrap();
    let router = ShardRouter::new(old, clients).unwrap();
    BinaryFixture {
        migrator,
        target,
        router,
        new_catalog: new,
    }
}

#[tokio::test]
async fn backfill_refuses_binary_source_before_copy() {
    let f = binary_fixture().await;
    let err = f.migrator.backfill().await.unwrap_err().to_string();
    assert!(
        err.contains("binary column") && err.contains("payload"),
        "{err}"
    );
    assert_eq!(row_count(&f.target).await, 0, "no row may be copied");
}

#[tokio::test]
async fn verify_refuses_binary_source() {
    let f = binary_fixture().await;
    let err = f.migrator.verify().await.unwrap_err().to_string();
    assert!(err.contains("binary column"), "{err}");
    assert!(!err.contains("run backfill first"), "{err}");
}

#[tokio::test]
async fn cutover_refuses_binary_source_and_keeps_routing() {
    let f = binary_fixture().await;
    let err = f
        .migrator
        .cutover(&f.router, f.new_catalog.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("binary column"), "{err}");
    assert_eq!(boundaries(&f.router.catalog()), vec![100]);
}

/// 模拟 MySQL/PostgreSQL 的 `information_schema` 结果；任何复制语句都算测试失败。
struct SchemaClient {
    dialect: Dialect,
    columns: Vec<(&'static str, &'static str)>,
}

#[async_trait]
impl SqlExecutor for SchemaClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database(format!(
            "preflight must not copy: {sql}"
        )))
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(format!(
            "preflight must not copy: {sql}"
        )))
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        assert!(sql.contains("information_schema"), "unexpected SQL: {sql}");
        assert_eq!(params.len(), 1);
        Ok(self
            .columns
            .iter()
            .map(|(name, type_name)| {
                Row::new(
                    vec!["name".into(), "data_type".into()],
                    vec![json!(name), json!(type_name)],
                )
            })
            .collect())
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

#[async_trait]
impl RdbmsClient for SchemaClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Err(RdbmsError::Database("unused in preflight test".into()))
    }
}

async fn schema_migrator(
    dialect: Dialect,
    columns: Vec<(&'static str, &'static str)>,
) -> (ShardMigrator, ShardRouter, ShardCatalog) {
    let source: Arc<dyn RdbmsClient> = Arc::new(SchemaClient { dialect, columns });
    let target = db().await;
    let state = db().await;
    let old = range_catalog(&[100], &["s0", "s1"]);
    let new = range_catalog(&[0], &["s0", "s1"]);
    let clients = client_map(&[("s0", &source), ("s1", &target)]);
    let migrator = ShardMigrator::new(
        ShardPlan {
            moves: vec![move_s0_to_s1()],
        },
        clients.clone(),
        state,
        old.clone(),
        new.clone(),
    )
    .unwrap();
    let router = ShardRouter::new(old, clients).unwrap();
    (migrator, router, new)
}

#[tokio::test]
async fn mysql_longblob_blocks_backfill() {
    let (migrator, _, _) = schema_migrator(
        Dialect::MySql,
        vec![("id", "bigint"), ("payload", "longblob")],
    )
    .await;
    let err = migrator.backfill().await.unwrap_err().to_string();
    assert!(
        err.contains("binary column") && err.contains("longblob"),
        "{err}"
    );
}

#[tokio::test]
async fn postgres_bytea_blocks_cutover() {
    let (migrator, router, new_catalog) = schema_migrator(
        Dialect::Postgres,
        vec![("id", "bigint"), ("payload", "bytea")],
    )
    .await;
    let err = migrator
        .cutover(&router, new_catalog)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("binary column") && err.contains("bytea"),
        "{err}"
    );
    assert_eq!(boundaries(&router.catalog()), vec![100]);
}

// ---- 双写 ----

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

    let guard = DualWrite::before_cutover(&f.old_router, &f.new_router);
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
        .for_key("orders", &json!(key))
        .unwrap();
    client.execute_with(INCREMENT, &[json!(key)]).await.unwrap();

    assert_eq!(
        amount(&f.s0, key).await,
        6,
        "same shard name in both topologies must be written once"
    );
}

#[tokio::test]
async fn dual_write_after_cutover_keeps_rollback_topology_current() {
    let f = dual_fixture().await;
    let key = key_for(&f.old, &f.new, 1, 2);
    insert_amount(&f.s1, key, 10).await;
    insert_amount(&f.s2, key, 100).await;

    DualWrite::before_cutover(&f.old_router, &f.new_router)
        .for_key("orders", &json!(key))
        .unwrap()
        .execute_with(INCREMENT, &[json!(key)])
        .await
        .unwrap();

    let after = DualWrite::after_cutover(&f.new_router, &f.old_router);
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
    let rolled_back = DualWrite::before_cutover(&f.old_router, &f.new_router);
    let rows = rolled_back
        .for_key("orders", &json!(key))
        .unwrap()
        .query_with("SELECT amount FROM orders WHERE id = ?", &[json!(key)])
        .await
        .unwrap();
    assert_eq!(rows[0].get("amount"), Some(&json!(12)));
}

#[tokio::test]
async fn dual_write_reports_mirror_failure_and_keeps_primary_write() {
    let f = dual_fixture().await;
    let key = key_for(&f.old, &f.new, 1, 2);
    insert_amount(&f.s1, key, 10).await;
    insert_amount(&f.s2, key, 100).await;
    f.s2.execute("DROP TABLE orders").await.unwrap();

    let client = DualWrite::before_cutover(&f.old_router, &f.new_router)
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

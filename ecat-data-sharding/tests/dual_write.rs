// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 双写路由/回滚语义：只有证明同一物理目标时才去重。
#![cfg(feature = "migrate")]

use std::collections::HashMap;
use std::sync::Arc;

use ecat_data::{RdbmsClient, SqlExecutor};
use ecat_data_sharding::{
    DualWrite, ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardRouter, ShardTable,
};
use ecat_data_sqlx::{PoolParams, SqlxClient};
use serde_json::json;

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

/// 单分片目录；`url` 用来证明两侧指向同一物理端点。
fn one_shard_catalog(name: &str, url: Option<&str>) -> ShardCatalog {
    ShardCatalog {
        tables: vec![ShardTable {
            name: "orders".into(),
            key: "id".into(),
            pk: "id".into(),
            algorithm: ShardAlgorithm::HashMod { shards: 1 },
            shards: vec![ShardEndpoint {
                name: name.into(),
                url: url.map(str::to_owned),
            }],
        }],
    }
}

fn router_for(catalog: &ShardCatalog, shard: &str, db: &Arc<dyn RdbmsClient>) -> ShardRouter {
    ShardRouter::new(catalog.clone(), client_map(&[(shard, db)])).unwrap()
}

/// 两个单分片拓扑，分片名和客户端都可不同。
struct OneShardFixture {
    old_db: Arc<dyn RdbmsClient>,
    new_db: Arc<dyn RdbmsClient>,
    old_router: ShardRouter,
    new_router: ShardRouter,
}

async fn one_shard_fixture(
    old: (&str, Option<&str>),
    new: (&str, Option<&str>),
    shared_client: bool,
) -> OneShardFixture {
    let old_db = db().await;
    create_orders(&old_db).await;
    let new_db = if shared_client {
        Arc::clone(&old_db)
    } else {
        let db = db().await;
        create_orders(&db).await;
        db
    };
    insert_amount(&old_db, 1, 10).await;
    if !shared_client {
        insert_amount(&new_db, 1, 100).await;
    }
    let old_catalog = one_shard_catalog(old.0, old.1);
    let new_catalog = one_shard_catalog(new.0, new.1);
    let old_router = router_for(&old_catalog, old.0, &old_db);
    let new_router = router_for(&new_catalog, new.0, &new_db);
    OneShardFixture {
        old_db,
        new_db,
        old_router,
        new_router,
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
async fn dual_write_same_name_distinct_clients_writes_both() {
    let f = one_shard_fixture(("s0", None), ("s0", None), false).await;

    DualWrite::before_cutover(&f.old_router, &f.new_router)
        .for_key("orders", &json!(1))
        .unwrap()
        .execute_with(INCREMENT, &[json!(1)])
        .await
        .unwrap();

    assert_eq!(amount(&f.old_db, 1).await, 11, "old topology primary write");
    assert_eq!(
        amount(&f.new_db, 1).await,
        101,
        "same shard name alone must not drop the mirror write"
    );
}

#[tokio::test]
async fn dual_write_equal_urls_write_once_even_with_distinct_clients() {
    let url = "mysql://proxy:3306/s0";
    let f = one_shard_fixture(("s0", Some(url)), ("s0", Some(url)), false).await;

    DualWrite::before_cutover(&f.old_router, &f.new_router)
        .for_key("orders", &json!(1))
        .unwrap()
        .execute_with(INCREMENT, &[json!(1)])
        .await
        .unwrap();

    assert_eq!(amount(&f.old_db, 1).await, 11, "primary written once");
    assert_eq!(
        amount(&f.new_db, 1).await,
        100,
        "equal non-empty URL proves the same physical target and must dedupe"
    );
}

#[tokio::test]
async fn dual_write_same_client_arc_writes_once_even_with_different_names() {
    let f = one_shard_fixture(("old0", None), ("new0", None), true).await;

    DualWrite::before_cutover(&f.old_router, &f.new_router)
        .for_key("orders", &json!(1))
        .unwrap()
        .execute_with(INCREMENT, &[json!(1)])
        .await
        .unwrap();

    assert_eq!(
        amount(&f.old_db, 1).await,
        11,
        "one client Arc is one physical target and must be written once"
    );
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

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 双写物理同一性：客户端 `Arc` 不同时，分片名相同或不同都必须显式选择策略。
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

fn client_map(entries: &[(&str, &Arc<dyn RdbmsClient>)]) -> HashMap<String, Arc<dyn RdbmsClient>> {
    entries
        .iter()
        .map(|(name, client)| ((*name).to_owned(), Arc::clone(client)))
        .collect()
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
    old_shard: &str,
    new_shard: &str,
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
    let old_catalog = one_shard_catalog(old_shard);
    let new_catalog = one_shard_catalog(new_shard);
    let old_router = router_for(&old_catalog, old_shard, &old_db);
    let new_router = router_for(&new_catalog, new_shard, &new_db);
    OneShardFixture {
        old_db,
        new_db,
        old_router,
        new_router,
    }
}

#[tokio::test]
async fn dual_write_same_name_distinct_clients_requires_policy() {
    let f = one_shard_fixture("s0", "s0", false).await;

    let err = DualWrite::before_cutover(&f.old_router, &f.new_router)
        .for_key("orders", &json!(1))
        .err()
        .expect("unproven same-name shards must error")
        .to_string();

    assert!(
        err.contains("assume_distinct()") && err.contains("assume_same()"),
        "{err}"
    );
    assert_eq!(amount(&f.old_db, 1).await, 10, "no primary write");
    assert_eq!(amount(&f.new_db, 1).await, 100, "no mirror write");
}

/// RED for the former name-shortcut bug: old `s0` and new `shard-0` can point at
/// the same physical database with different clients, so differing names prove nothing.
#[tokio::test]
async fn dual_write_different_names_distinct_clients_require_policy() {
    let f = one_shard_fixture("s0", "shard-0", false).await;

    let err = DualWrite::before_cutover(&f.old_router, &f.new_router)
        .for_key("orders", &json!(1))
        .err()
        .expect("different shard names must not be treated as proof of distinct targets")
        .to_string();

    assert!(
        err.contains("`s0`") && err.contains("`shard-0`"),
        "error must name both shards: {err}"
    );
    assert!(
        err.contains("old topology") && err.contains("new topology"),
        "{err}"
    );
    assert!(
        err.contains("assume_distinct()") && err.contains("assume_same()"),
        "{err}"
    );
    assert_eq!(amount(&f.old_db, 1).await, 10, "no primary write");
    assert_eq!(amount(&f.new_db, 1).await, 100, "no mirror write");
}

#[tokio::test]
async fn dual_write_assume_distinct_writes_both_with_different_names() {
    let f = one_shard_fixture("s0", "shard-0", false).await;

    DualWrite::before_cutover(&f.old_router, &f.new_router)
        .assume_distinct()
        .for_key("orders", &json!(1))
        .unwrap()
        .execute_with(INCREMENT, &[json!(1)])
        .await
        .unwrap();

    assert_eq!(amount(&f.old_db, 1).await, 11, "old topology primary write");
    assert_eq!(
        amount(&f.new_db, 1).await,
        101,
        "assume_distinct writes mirror"
    );
}

#[tokio::test]
async fn dual_write_assume_same_writes_primary_only_with_different_names() {
    let f = one_shard_fixture("s0", "shard-0", false).await;

    let client = DualWrite::before_cutover(&f.old_router, &f.new_router)
        .assume_same()
        .for_key("orders", &json!(1))
        .unwrap();
    client.execute_with(INCREMENT, &[json!(1)]).await.unwrap();

    assert_eq!(amount(&f.old_db, 1).await, 11, "primary write");
    assert_eq!(amount(&f.new_db, 1).await, 100, "assume_same skips mirror");
    assert!(client.mirror_client().is_none());
}

#[tokio::test]
async fn dual_write_same_client_arc_writes_once_even_with_different_names() {
    let f = one_shard_fixture("old0", "new0", true).await;

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

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 目录校验与路由测试。
use serde_json::{Value, json};

use crate::config::{ShardingConfig, ShardingMode};
use crate::{ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardTable, stable_hash64};

fn endpoint(name: &str) -> ShardEndpoint {
    ShardEndpoint {
        name: name.to_owned(),
        url: None,
    }
}

fn table(name: &str, algorithm: ShardAlgorithm, shard_names: &[&str]) -> ShardTable {
    ShardTable {
        name: name.to_owned(),
        key: "id".to_owned(),
        pk: "id".to_owned(),
        algorithm,
        shards: shard_names.iter().map(|name| endpoint(name)).collect(),
    }
}

fn hash_table(name: &str) -> ShardTable {
    table(
        name,
        ShardAlgorithm::HashMod { shards: 4 },
        &["s0", "s1", "s2", "s3"],
    )
}

fn catalog(tables: Vec<ShardTable>) -> ShardCatalog {
    ShardCatalog { tables }
}

#[test]
fn catalog_rejects_duplicate_table_names() {
    let catalog = catalog(vec![hash_table("users"), hash_table("users")]);
    assert!(matches!(catalog.validate(), Err(ShardError::Config(_))));
}

#[test]
fn catalog_rejects_empty_shards() {
    let catalog = catalog(vec![table(
        "users",
        ShardAlgorithm::HashMod { shards: 0 },
        &[],
    )]);
    assert!(matches!(catalog.validate(), Err(ShardError::Config(_))));
}

#[test]
fn hash_mod_routes_same_key_to_same_shard() {
    let catalog = catalog(vec![hash_table("users")]);
    assert_eq!(catalog.validate(), Ok(()));
    let first = catalog.shard_for("users", &json!("alice")).unwrap();
    let second = catalog.shard_for("users", &json!("alice")).unwrap();
    assert_eq!(first, second);
    assert!(first < 4);
}

#[test]
fn range_routes_boundaries() {
    let catalog = catalog(vec![table(
        "events",
        ShardAlgorithm::Range {
            boundaries: vec![100, 200],
        },
        &["a", "b", "c"],
    )]);
    assert_eq!(catalog.validate(), Ok(()));
    assert_eq!(catalog.shard_for("events", &json!(99)).unwrap(), 0);
    // 边界值属于上界那一侧的分片。
    assert_eq!(catalog.shard_for("events", &json!(100)).unwrap(), 1);
    assert_eq!(catalog.shard_for("events", &json!(200)).unwrap(), 2);
    // 区间算法不接受非整型键。
    assert!(matches!(
        catalog.shard_for("events", &json!("100")),
        Err(ShardError::Config(_))
    ));
}

#[test]
fn unknown_table_is_error() {
    let catalog = catalog(vec![hash_table("users")]);
    assert_eq!(catalog.table("users").unwrap().name, "users");
    assert_eq!(
        catalog.table("orders").unwrap_err(),
        ShardError::UnknownTable("orders".to_owned())
    );
    assert_eq!(
        catalog.shard_for("orders", &json!("alice")),
        Err(ShardError::UnknownTable("orders".to_owned()))
    );
}

#[test]
fn null_key_is_error() {
    let catalog = catalog(vec![hash_table("users")]);
    assert_eq!(
        catalog.shard_for("users", &Value::Null),
        Err(ShardError::MissingKey)
    );
    assert_eq!(stable_hash64(&Value::Null), Err(ShardError::MissingKey));
}

#[test]
fn catalog_hash_fnv1a_known_vectors() {
    // FNV-1a 64 位参考向量。
    assert_eq!(stable_hash64(&json!("")).unwrap(), 0xcbf2_9ce4_8422_2325);
    assert_eq!(stable_hash64(&json!("a")).unwrap(), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(
        stable_hash64(&json!("foobar")).unwrap(),
        0x8594_4171_f739_67e8
    );
}

#[test]
fn catalog_hash_is_order_independent_for_object_keys() {
    let first = json!({"tenant": 1, "user": 2});
    let second = json!({"user": 2, "tenant": 1});
    assert_eq!(stable_hash64(&first), stable_hash64(&second));
}

#[test]
fn catalog_rejects_shard_count_mismatch() {
    let hash = catalog(vec![table(
        "users",
        ShardAlgorithm::HashMod { shards: 2 },
        &["s0", "s1", "s2"],
    )]);
    assert_eq!(hash.validate(), Err(ShardError::ShardCountMismatch));
    // 未经 validate 的目录也不能路由到不存在的分片。
    assert_eq!(
        hash.shard_for("users", &json!("alice")),
        Err(ShardError::ShardCountMismatch)
    );

    let range = catalog(vec![table(
        "events",
        ShardAlgorithm::Range {
            boundaries: vec![100],
        },
        &["a", "b", "c"],
    )]);
    assert_eq!(range.validate(), Err(ShardError::ShardCountMismatch));
}

#[test]
fn catalog_rejects_duplicate_endpoint_names() {
    let catalog = catalog(vec![table(
        "users",
        ShardAlgorithm::HashMod { shards: 2 },
        &["s0", "s0"],
    )]);
    assert!(matches!(catalog.validate(), Err(ShardError::Config(_))));
}

#[test]
fn catalog_rejects_empty_table_fields() {
    let mut users = hash_table("users");
    users.key.clear();
    assert!(matches!(
        catalog(vec![users]).validate(),
        Err(ShardError::Config(_))
    ));
}

#[test]
fn range_rejects_unsorted_boundaries() {
    let catalog = catalog(vec![table(
        "events",
        ShardAlgorithm::Range {
            boundaries: vec![200, 100],
        },
        &["a", "b", "c"],
    )]);
    assert!(matches!(catalog.validate(), Err(ShardError::Config(_))));
}

#[test]
fn catalog_client_mode_requires_endpoint_urls() {
    let mut users = hash_table("users");
    let client = ShardingConfig {
        mode: ShardingMode::Client,
        catalog: catalog(vec![users.clone()]),
    };
    assert!(matches!(client.validate(), Err(ShardError::Config(_))));

    // 正向对照：每个端点都有 url 后通过。
    for shard in &mut users.shards {
        shard.url = Some(format!("mysql://{}/db", shard.name));
    }
    let client = ShardingConfig {
        mode: ShardingMode::Client,
        catalog: catalog(vec![users]),
    };
    assert_eq!(client.validate(), Ok(()));
}

#[test]
fn catalog_proxy_mode_forbids_endpoint_urls() {
    let mut users = hash_table("users");
    let proxy = ShardingConfig {
        mode: ShardingMode::Proxy,
        catalog: catalog(vec![users.clone()]),
    };
    assert_eq!(proxy.validate(), Ok(()));

    // 反向对照：代理模式下逐分片 url 非法。
    users.shards[0].url = Some("mysql://s0/db".to_owned());
    let proxy = ShardingConfig {
        mode: ShardingMode::Proxy,
        catalog: catalog(vec![users]),
    };
    assert!(matches!(proxy.validate(), Err(ShardError::Config(_))));
}

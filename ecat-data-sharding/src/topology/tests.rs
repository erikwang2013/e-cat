// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 拓扑源与热更新测试：swap 后新键走新目录，旧 KeyedClient 保持旧快照。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use serde_json::json;

use crate::{
    ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardRouter, ShardTable, StaticTopology,
    TopologySource,
};

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, shard: &str) {
        self.0.lock().unwrap().push(shard.to_owned());
    }
    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct Fake {
    name: &'static str,
    log: Log,
}

#[async_trait]
impl SqlExecutor for Fake {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        self.log.push(self.name);
        Ok(1)
    }
    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.log.push(self.name);
        Ok(vec![Row::new(
            vec!["shard".to_owned()],
            vec![json!(self.name)],
        )])
    }
    fn dialect(&self) -> Dialect {
        Dialect::MySql
    }
}

#[async_trait]
impl RdbmsClient for Fake {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Ok(Transaction::new())
    }
}

fn table(name: &str, shards: &[&str]) -> ShardTable {
    ShardTable {
        name: name.to_owned(),
        key: "id".to_owned(),
        pk: "id".to_owned(),
        algorithm: ShardAlgorithm::HashMod {
            shards: shards.len() as u32,
        },
        shards: shards
            .iter()
            .map(|name| ShardEndpoint {
                name: (*name).to_owned(),
                url: None,
            })
            .collect(),
    }
}

fn catalog(shards: &[&str]) -> ShardCatalog {
    ShardCatalog {
        tables: vec![table("users", shards)],
    }
}

fn router(old: &[&str], clients: &[&'static str]) -> (ShardRouter, Log) {
    let log = Log::default();
    let clients = clients
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                Arc::new(Fake {
                    name,
                    log: log.clone(),
                }) as Arc<dyn RdbmsClient>,
            )
        })
        .collect::<HashMap<_, _>>();
    let router = ShardRouter::new(catalog(old), clients).unwrap();
    (router, log)
}

async fn shard_of(client: &crate::KeyedClient) -> String {
    let rows = client.query("SELECT 1").await.unwrap();
    rows[0].get("shard").unwrap().as_str().unwrap().to_owned()
}

#[tokio::test]
async fn static_topology_loads_catalog() {
    let source = StaticTopology::new(catalog(&["s0", "s1"]));
    let loaded = source.load().await.unwrap();
    assert_eq!(loaded.tables.len(), 1);
    assert_eq!(loaded.tables[0].name, "users");
    assert_eq!(loaded.tables[0].shards[0].name, "s0");
}

#[tokio::test]
async fn reload_routes_new_key_to_new_shard() {
    let (router, log) = router(&["s0", "s1"], &["s0", "s1", "s2", "s3"]);
    let before = shard_of(&router.for_key("users", &json!("alice")).unwrap()).await;
    assert!(before == "s0" || before == "s1");

    router.reload(catalog(&["s2", "s3"])).unwrap();
    let after = shard_of(&router.for_key("users", &json!("alice")).unwrap()).await;
    assert!(after == "s2" || after == "s3", "got {after}");
    assert_ne!(before, after);
    assert_eq!(log.entries().len(), 2);
}

#[tokio::test]
async fn reload_rejects_invalid_catalog() {
    let (router, _) = router(&["s0", "s1"], &["s0", "s1", "s2", "s3"]);
    let mut invalid = catalog(&["s2", "s3"]);
    invalid.tables.push(table("users", &["s2", "s3"])); // duplicate table name
    assert!(router.reload(invalid).is_err());
    assert_eq!(router.catalog().tables[0].shards[0].name, "s0");
}

#[tokio::test]
async fn reload_rejects_missing_client() {
    let (router, _) = router(&["s0", "s1"], &["s0", "s1"]);
    assert!(router.reload(catalog(&["s9"])).is_err());
    assert_eq!(router.catalog().tables[0].shards[0].name, "s0");
}

#[tokio::test]
async fn in_flight_keyed_client_keeps_old_snapshot() {
    let (router, _) = router(&["s0", "s1"], &["s0", "s1", "s2", "s3"]);
    let old_client = router.for_key("users", &json!("alice")).unwrap();
    router.reload(catalog(&["s2", "s3"])).unwrap();

    let old_shard = shard_of(&old_client).await;
    assert!(old_shard == "s0" || old_shard == "s1", "got {old_shard}");
    let new_shard = shard_of(&router.for_key("users", &json!("alice")).unwrap()).await;
    assert!(new_shard == "s2" || new_shard == "s3", "got {new_shard}");
}

#[tokio::test]
async fn catalog_reflects_reload() {
    let (router, _) = router(&["s0", "s1"], &["s0", "s1", "s2", "s3"]);
    assert_eq!(router.catalog().tables[0].shards[0].name, "s0");
    router.reload(catalog(&["s2", "s3"])).unwrap();
    assert_eq!(router.catalog().tables[0].shards[0].name, "s2");
}

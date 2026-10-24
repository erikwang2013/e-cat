// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 键绑定路由测试：六个方法 + 事务落到同一个分片；错误路径显式报错。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use serde_json::{Value, json};

use crate::{ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardRouter, ShardTable};

const SHARDS: [&str; 4] = ["s0", "s1", "s2", "s3"];
const DIALECTS: [Dialect; 4] = [
    Dialect::Sqlite,
    Dialect::Postgres,
    Dialect::MySql,
    Dialect::Mssql,
];

/// 共享调用日志：`"分片名:方法"`；`dialect()` 是本地判断、不记。
#[derive(Clone)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn push(&self, shard: &str, method: &str) {
        self.0.lock().unwrap().push(format!("{shard}:{method}"));
    }

    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// 假分片客户端：记录每次调用，返回与分片绑定的结果。
struct FakeClient {
    name: &'static str,
    dialect: Dialect,
    log: Log,
}

impl FakeClient {
    fn shared(name: &'static str, dialect: Dialect, log: &Log) -> Arc<dyn RdbmsClient> {
        Arc::new(Self {
            name,
            dialect,
            log: log.clone(),
        })
    }

    fn record<T>(&self, method: &str, ok: T) -> Result<T, RdbmsError> {
        self.log.push(self.name, method);
        Ok(ok)
    }
}

#[async_trait]
impl SqlExecutor for FakeClient {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        self.record("execute", 11)
    }
    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.record("query", self.rows())
    }
    async fn execute_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.record("execute_with", 12)
    }
    async fn query_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.record("query_with", self.rows())
    }
    async fn query_write(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.record("query_write", self.rows())
    }
    async fn execute_then_query(
        &self,
        _first: &str,
        _first_params: &[serde_json::Value],
        _second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.record("execute_then_query", self.rows())
    }
    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

impl FakeClient {
    fn rows(&self) -> Vec<Row> {
        vec![Row::new(vec!["shard".to_owned()], vec![json!(self.name)])]
    }
}

#[async_trait]
impl RdbmsClient for FakeClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.log.push(self.name, "transaction");
        Ok(Transaction::new())
    }
}

fn table(name: &str, algorithm: ShardAlgorithm, shard_names: &[&str]) -> ShardTable {
    ShardTable {
        name: name.to_owned(),
        key: "id".to_owned(),
        pk: "id".to_owned(),
        algorithm,
        shards: shard_names
            .iter()
            .map(|name| ShardEndpoint {
                name: (*name).to_owned(),
                url: None,
            })
            .collect(),
    }
}

fn hash_table(name: &str) -> ShardTable {
    table(
        name,
        ShardAlgorithm::HashMod {
            shards: SHARDS.len() as u32,
        },
        &SHARDS,
    )
}

/// 四个分片各有独立客户端；返回目录以便测试自己算出期望分片。
fn router(log: &Log) -> (ShardRouter, ShardCatalog) {
    let clients: HashMap<String, Arc<dyn RdbmsClient>> = SHARDS
        .iter()
        .zip(DIALECTS)
        .map(|(name, dialect)| ((*name).to_owned(), FakeClient::shared(name, dialect, log)))
        .collect();
    let catalog = ShardCatalog {
        tables: vec![hash_table("users")],
    };
    let router = ShardRouter::new(catalog.clone(), clients).unwrap();
    (router, catalog)
}

#[tokio::test]
async fn for_key_routes_all_six_methods_to_same_shard() {
    let log = Log::new();
    let (router, catalog) = router(&log);
    let key = json!("alice");
    let index = catalog.shard_for("users", &key).unwrap();
    let shard = SHARDS[index];

    let keyed = router.for_key("users", &key).unwrap();
    assert_eq!(keyed.table(), "users");
    assert_eq!(keyed.dialect(), DIALECTS[index]);

    keyed.execute("SELECT 1").await.unwrap();
    keyed.query("SELECT 1").await.unwrap();
    keyed.execute_with("SELECT ?", &[]).await.unwrap();
    keyed.query_with("SELECT ?", &[]).await.unwrap();
    keyed.query_write("INSERT ...", &[]).await.unwrap();
    keyed
        .execute_then_query("INSERT ...", &[], "SELECT LAST_INSERT_ID()")
        .await
        .unwrap();

    // 六个方法各留下一笔、且全部落在同一个分片：漏写任何一个 override，
    // 要么走默认实现报错、要么记在别的分片名下。
    assert_eq!(
        log.entries(),
        vec![
            format!("{shard}:execute"),
            format!("{shard}:query"),
            format!("{shard}:execute_with"),
            format!("{shard}:query_with"),
            format!("{shard}:query_write"),
            format!("{shard}:execute_then_query"),
        ]
    );
}

#[tokio::test]
async fn for_key_transaction_uses_same_shard() {
    let log = Log::new();
    let (router, catalog) = router(&log);
    let key = json!("bob");
    let shard = SHARDS[catalog.shard_for("users", &key).unwrap()];

    router
        .for_key("users", &key)
        .unwrap()
        .transaction()
        .await
        .unwrap();

    assert_eq!(log.entries(), vec![format!("{shard}:transaction")]);
}

#[test]
fn unknown_table_is_error() {
    let (router, _) = router(&Log::new());
    assert!(matches!(
        router.for_key("missing", &json!("alice")),
        Err(ShardError::UnknownTable(name)) if name == "missing"
    ));
}

#[test]
fn missing_key_is_error() {
    let (router, _) = router(&Log::new());
    assert!(matches!(
        router.for_key("users", &Value::Null),
        Err(ShardError::MissingKey)
    ));
}

#[test]
fn shard_count_mismatch_is_error() {
    let log = Log::new();
    let clients: HashMap<String, Arc<dyn RdbmsClient>> = SHARDS[..3]
        .iter()
        .zip(DIALECTS)
        .map(|(name, dialect)| ((*name).to_owned(), FakeClient::shared(name, dialect, &log)))
        .collect();
    // 算法声明 4 片，端点只有 3 个。
    let catalog = ShardCatalog {
        tables: vec![table(
            "users",
            ShardAlgorithm::HashMod { shards: 4 },
            &SHARDS[..3],
        )],
    };
    assert!(matches!(
        ShardRouter::new(catalog, clients),
        Err(ShardError::ShardCountMismatch)
    ));
}

#[test]
fn missing_client_is_error() {
    let log = Log::new();
    let clients: HashMap<String, Arc<dyn RdbmsClient>> = SHARDS[..3]
        .iter()
        .zip(DIALECTS)
        .map(|(name, dialect)| ((*name).to_owned(), FakeClient::shared(name, dialect, &log)))
        .collect();
    assert!(matches!(
        ShardRouter::new(
            ShardCatalog {
                tables: vec![hash_table("users")],
            },
            clients,
        ),
        Err(ShardError::Config(message)) if message.contains("s3")
    ));
}

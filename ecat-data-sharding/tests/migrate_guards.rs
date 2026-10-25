// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 迁移保护：二进制列在任何复制/切流前 fail closed。
#![cfg(feature = "migrate")]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use ecat_data_sharding::{
    KeyRange, ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardMigrator, ShardMove, ShardPlan,
    ShardRouter, ShardTable,
};
use ecat_data_sqlx::{PoolParams, SqlxClient};
use serde_json::{Value, json};

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

async fn row_count(db: &Arc<dyn RdbmsClient>) -> i64 {
    db.query("SELECT COUNT(*) AS n FROM orders").await.unwrap()[0]
        .get("n")
        .unwrap()
        .as_i64()
        .unwrap()
}

fn table_named(name: &str, algorithm: ShardAlgorithm, shards: &[&str]) -> ShardTable {
    ShardTable {
        name: name.into(),
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

fn range_catalog_named(table: &str, boundaries: &[i64], shards: &[&str]) -> ShardCatalog {
    ShardCatalog {
        tables: vec![table_named(
            table,
            ShardAlgorithm::Range {
                boundaries: boundaries.to_vec(),
            },
            shards,
        )],
    }
}

fn range_catalog(boundaries: &[i64], shards: &[&str]) -> ShardCatalog {
    range_catalog_named("orders", boundaries, shards)
}

fn client_map(entries: &[(&str, &Arc<dyn RdbmsClient>)]) -> HashMap<String, Arc<dyn RdbmsClient>> {
    entries
        .iter()
        .map(|(name, client)| ((*name).to_owned(), Arc::clone(client)))
        .collect()
}

fn move_named(table: &str) -> ShardMove {
    ShardMove {
        table: table.into(),
        from: "s0".into(),
        to: "s1".into(),
        key_range: Some(KeyRange {
            low: Some(0),
            high: Some(100),
        }),
    }
}

fn move_s0_to_s1() -> ShardMove {
    move_named("orders")
}

fn boundaries(catalog: &ShardCatalog) -> Vec<i64> {
    match &catalog.tables[0].algorithm {
        ShardAlgorithm::Range { boundaries } => boundaries.clone(),
        other => panic!("expected range algorithm, got {other:?}"),
    }
}

// ---- 二进制列预检 ----

const MYSQL_UNQUALIFIED: &str = concat!(
    "SELECT COLUMN_NAME AS name, DATA_TYPE AS data_type ",
    "FROM information_schema.COLUMNS ",
    "WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?"
);
const MYSQL_QUALIFIED: &str = concat!(
    "SELECT COLUMN_NAME AS name, DATA_TYPE AS data_type ",
    "FROM information_schema.COLUMNS ",
    "WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?"
);
const POSTGRES_UNQUALIFIED: &str = concat!(
    "SELECT column_name AS name, data_type AS data_type ",
    "FROM information_schema.columns ",
    "WHERE table_name = $1 ",
    "AND table_schema = ANY (current_schemas(false))"
);
const POSTGRES_QUALIFIED: &str = concat!(
    "SELECT column_name AS name, data_type AS data_type ",
    "FROM information_schema.columns ",
    "WHERE table_schema = $1 AND table_name = $2"
);
const SQLITE_UNQUALIFIED: &str = "SELECT name, type AS data_type FROM pragma_table_info(?)";
const SQLITE_QUALIFIED: &str = "SELECT name, type AS data_type FROM pragma_table_info(?, ?)";

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
    expected_sql: &'static str,
    expected_params: Vec<&'static str>,
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
        assert_eq!(sql, self.expected_sql);
        let expected: Vec<Value> = self
            .expected_params
            .iter()
            .map(|param| Value::String((*param).to_owned()))
            .collect();
        assert_eq!(params, expected.as_slice());
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
    table: &str,
    columns: Vec<(&'static str, &'static str)>,
    expected_sql: &'static str,
    expected_params: &[&'static str],
) -> (ShardMigrator, ShardRouter, ShardCatalog) {
    let source: Arc<dyn RdbmsClient> = Arc::new(SchemaClient {
        dialect,
        columns,
        expected_sql,
        expected_params: expected_params.to_vec(),
    });
    let target = db().await;
    let state = db().await;
    let old = range_catalog_named(table, &[100], &["s0", "s1"]);
    let new = range_catalog_named(table, &[0], &["s0", "s1"]);
    let clients = client_map(&[("s0", &source), ("s1", &target)]);
    let migrator = ShardMigrator::new(
        ShardPlan {
            moves: vec![move_named(table)],
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
        "orders",
        vec![("id", "bigint"), ("payload", "longblob")],
        MYSQL_UNQUALIFIED,
        &["orders"],
    )
    .await;
    let err = migrator.backfill().await.unwrap_err().to_string();
    assert!(
        err.contains("binary column") && err.contains("longblob"),
        "{err}"
    );
}

#[tokio::test]
async fn mysql_qualified_table_blocks_backfill() {
    let (migrator, _, _) = schema_migrator(
        Dialect::MySql,
        "shop.orders",
        vec![("id", "bigint"), ("payload", "blob")],
        MYSQL_QUALIFIED,
        &["shop", "orders"],
    )
    .await;
    let err = migrator.backfill().await.unwrap_err().to_string();
    assert!(
        err.contains("binary column") && err.contains("payload"),
        "{err}"
    );
}

#[tokio::test]
async fn postgres_bytea_blocks_cutover() {
    let (migrator, router, new_catalog) = schema_migrator(
        Dialect::Postgres,
        "orders",
        vec![("id", "bigint"), ("payload", "bytea")],
        POSTGRES_UNQUALIFIED,
        &["orders"],
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

#[tokio::test]
async fn postgres_qualified_table_blocks_cutover() {
    let (migrator, router, new_catalog) = schema_migrator(
        Dialect::Postgres,
        "public.orders",
        vec![("payload", "bytea")],
        POSTGRES_QUALIFIED,
        &["public", "orders"],
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

#[tokio::test]
async fn sqlite_unqualified_table_uses_one_param() {
    let (migrator, _, _) = schema_migrator(
        Dialect::Sqlite,
        "orders",
        Vec::new(),
        SQLITE_UNQUALIFIED,
        &["orders"],
    )
    .await;
    let err = migrator.backfill().await.unwrap_err().to_string();
    assert!(err.contains("cannot inspect source schema"), "{err}");
}

#[tokio::test]
async fn sqlite_qualified_table_uses_table_then_schema_params() {
    let (migrator, _, _) = schema_migrator(
        Dialect::Sqlite,
        "main.orders",
        Vec::new(),
        SQLITE_QUALIFIED,
        &["orders", "main"],
    )
    .await;
    let err = migrator.backfill().await.unwrap_err().to_string();
    assert!(err.contains("cannot inspect source schema"), "{err}");
}

#[tokio::test]
async fn backfill_accepts_qualified_sqlite_table_name() {
    let source = db().await;
    source
        .execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER)")
        .await
        .unwrap();
    source
        .execute("INSERT INTO orders (id, amount) VALUES (1, 10)")
        .await
        .unwrap();
    let target = db().await;
    target
        .execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER)")
        .await
        .unwrap();
    let state = db().await;
    let old = range_catalog_named("main.orders", &[100], &["s0", "s1"]);
    let new = range_catalog_named("main.orders", &[0], &["s0", "s1"]);
    let clients = client_map(&[("s0", &source), ("s1", &target)]);
    let migrator = ShardMigrator::new(
        ShardPlan {
            moves: vec![move_named("main.orders")],
        },
        clients,
        state,
        old,
        new,
    )
    .unwrap();

    let report = migrator.backfill().await.unwrap();
    assert_eq!(report.moved_rows, 1);
    assert_eq!(row_count(&target).await, 1);
}

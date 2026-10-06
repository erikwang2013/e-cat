// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `Migrator` 的端到端断言：真跑内存 SQLite（假 executor 答不了
//! 「表真的建出来了吗」），以及用 Spy 断言 MSSQL 那条谁也没法真跑的路径。

use super::*;
use crate::crud::fixtures::{U, mem_sqlite, spy};
use crate::entity::Entity;
use crate::error::OrmError;
use ecat_data::{Dialect, Row, SqlExecutor};
use serde_json::json;

fn make(name: &'static str) -> MigrationBuilder {
    MigrationBuilder::new(move |_| format!("CREATE TABLE {name} (x INTEGER)"))
}

#[tokio::test]
async fn status_lists_everything_as_pending_on_a_fresh_database() {
    let db = mem_sqlite("migrate-status-fresh").await;
    let m = Migrator::new(&db).add("001_users", create_table::<U>());
    let s = m.status().await.unwrap();
    assert_eq!(s.pending, vec![1]);
    assert!(s.applied.is_empty());
    assert!(s.unknown.is_empty());
}

#[tokio::test]
async fn run_creates_the_table_and_records_the_version() {
    let db = mem_sqlite("migrate-run").await;
    let m = Migrator::new(&db).add("001_users", create_table::<U>());
    m.run().await.unwrap();
    // 表真的建出来了：插一行再读回来
    db.execute("INSERT INTO users (name) VALUES ('a')")
        .await
        .unwrap();
    let rows = db.query("SELECT name FROM users").await.unwrap();
    assert_eq!(rows.len(), 1);
    let s = m.status().await.unwrap();
    assert_eq!(s.applied, vec![1]);
    assert!(s.pending.is_empty());
    // 再跑一次**不重复执行**，版本行也不重复
    m.run().await.unwrap();
    let rows = db
        .query("SELECT version FROM _ecat_migrations")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "重复 run 不得重复记录版本");
}

/// **单个失败即中止**：第二条炸了，第三条**不能被执行** —— 半应用的迁移集
/// 比一条都没应用更难排查。
#[tokio::test]
async fn run_aborts_at_the_first_failure_and_leaves_later_ones_pending() {
    let db = mem_sqlite("migrate-abort").await;
    let m = Migrator::new(&db)
        .add("001_ok", make("a"))
        .add(
            "002_bad",
            MigrationBuilder::new(|_| "THIS IS NOT SQL".into()),
        )
        .add("003_later", make("c"));
    let e = m.run().await.unwrap_err();
    assert!(
        matches!(e, OrmError::Migration { version: 2, .. }),
        "错误必须带上失败的版本号，得到 {e:?}"
    );
    assert!(
        db.query("SELECT 1 FROM c").await.is_err(),
        "第二条失败后第三条不得被执行"
    );
    let s = m.status().await.unwrap();
    assert_eq!(s.applied, vec![1], "第一条已应用");
    assert_eq!(s.pending, vec![2, 3], "第二条与第三条都还 pending");
}

/// 版本号决定执行顺序，**不是声明顺序**：先声明 002 再声明 001，
/// 001 仍要先跑（否则建表顺序会依赖文件里谁写在前面）。
#[tokio::test]
async fn run_executes_in_version_order_not_declaration_order() {
    let db = mem_sqlite("migrate-order").await;
    let m = Migrator::new(&db)
        .add("002_second", make("b"))
        .add("001_first", make("a"));
    m.run().await.unwrap();
    assert_eq!(m.status().await.unwrap().applied, vec![1, 2]);
}

/// 名字解析不出数字前缀时**报错**，且一条都不执行。
#[tokio::test]
async fn a_bad_migration_name_errors_before_anything_runs() {
    let db = mem_sqlite("migrate-badname").await;
    let m = Migrator::new(&db).add("users", make("a"));
    let e = m.status().await.unwrap_err();
    assert!(matches!(e, OrmError::InvalidMigrationName(_)), "got: {e:?}");
    assert!(
        db.query("SELECT 1 FROM a").await.is_err(),
        "什么都没该被执行"
    );
}

#[tokio::test]
async fn down_on_an_unapplied_version_errors() {
    let db = mem_sqlite("migrate-down-unapplied").await;
    let m = Migrator::new(&db).add("001_users", create_table::<U>());
    let e = m.down(1).await.unwrap_err();
    assert!(matches!(e, OrmError::MigrationNotApplied(1)), "got: {e:?}");
}

#[tokio::test]
async fn down_runs_the_reverse_sql_and_removes_the_version_row() {
    let db = mem_sqlite("migrate-down").await;
    let m = Migrator::new(&db).add(
        "001_users",
        create_table::<U>().with_reverse(|d| drop_table_sql(U::META, d)),
    );
    m.run().await.unwrap();
    m.down(1).await.unwrap();
    assert!(
        db.query("SELECT 1 FROM users").await.is_err(),
        "反向 SQL 应已把表删掉"
    );
    let s = m.status().await.unwrap();
    assert_eq!(s.pending, vec![1], "版本行应从版本表删掉");
    assert!(s.applied.is_empty());
}

/// 没有反向 SQL 时 `down` 报 `MigrationIrreversible`，且**版本行必须还在**
/// —— 报错却顺手把版本删了，帐就永远对不上。
#[tokio::test]
async fn down_without_reverse_sql_keeps_the_version_row() {
    let db = mem_sqlite("migrate-down-irreversible").await;
    let m = Migrator::new(&db).add("001_users", create_table::<U>());
    m.run().await.unwrap();
    let e = m.down(1).await.unwrap_err();
    assert!(
        matches!(e, OrmError::MigrationIrreversible(_)),
        "got: {e:?}"
    );
    assert_eq!(m.status().await.unwrap().applied, vec![1]);
}

/// 工厂版 `drop_table::<E>()` 一样要能用。
#[tokio::test]
async fn drop_table_factory_removes_the_table() {
    let db = mem_sqlite("migrate-drop-factory").await;
    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    Migrator::new(&db)
        .add("001_drop_users", drop_table::<U>())
        .run()
        .await
        .unwrap();
    assert!(db.query("SELECT 1 FROM users").await.is_err());
}

/// 版本表的建表走的是**同一套** DDL 生成器（改名后的
/// [`DialectSpec::create_table_prefix`]）：整串断言，防止有人另写一份。
#[tokio::test]
async fn version_table_ddl_is_generated_by_the_same_ddl_layer() {
    let spy = spy(Dialect::Sqlite, 0);
    Migrator::new(&spy).status().await.unwrap();
    let (sql, _) = spy.calls().first().cloned().expect("应有建表语句");
    assert_eq!(
        sql,
        "CREATE TABLE IF NOT EXISTS \"_ecat_migrations\" (\"version\" INTEGER PRIMARY KEY, \
         \"name\" TEXT NOT NULL, \"applied_at\" TEXT NOT NULL)"
    );
}

/// MSSQL：先查 `INFORMATION_SCHEMA.TABLES`（表名走绑定参数，不拼 SQL），
/// 且建表前缀是完整的 `CREATE TABLE [...]` —— 不是旧实现那个空串。
#[tokio::test]
async fn mssql_checks_information_schema_before_creating_the_version_table() {
    let spy = spy(Dialect::Mssql, 0);
    Migrator::new(&spy).status().await.unwrap();
    let calls = spy.calls();
    assert_eq!(
        calls[0],
        (
            "SELECT 1 FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_NAME = @P1".to_string(),
            vec![json!("_ecat_migrations")],
        )
    );
    assert!(
        calls[1].0.starts_with("CREATE TABLE [_ecat_migrations] ("),
        "前缀必须是完整语句: {}",
        calls[1].0
    );
}

/// 表已存在时**跳过建表**：多建一次在真库上是硬报错。
#[tokio::test]
async fn mssql_skips_the_create_when_the_table_already_exists() {
    let spy = spy(Dialect::Mssql, 0);
    spy.rows
        .lock()
        .unwrap()
        .push(Row::new(vec!["version".into()], vec![json!(7)]));
    let s = Migrator::new(&spy)
        .add("001_users", create_table::<U>())
        .status()
        .await
        .unwrap();
    assert_eq!(
        s.unknown,
        vec![7],
        "版本表里的 7 没在迁移列表里，必须报出来"
    );
    let calls = spy.calls();
    assert_eq!(calls.len(), 2, "只有「查存在性 + 读版本」两问: {calls:?}");
    assert!(
        calls.iter().all(|(s, _)| !s.starts_with("CREATE TABLE [")),
        "不得建表"
    );
}

/// 工厂在**连接自己的方言**上生成 SQL，而不是绑定某个方言。
#[tokio::test]
async fn a_factory_generates_sql_for_the_connection_dialect() {
    let spy = spy(Dialect::Mssql, 0);
    Migrator::new(&spy)
        .add("001_users", create_table::<U>())
        .run()
        .await
        .unwrap();
    let sqls: Vec<String> = spy.calls().into_iter().map(|(s, _)| s).collect();
    assert!(
        sqls.iter().any(|s| s.starts_with("CREATE TABLE [users] (")),
        "got: {sqls:?}"
    );
    assert!(
        !sqls.iter().any(|s| s.contains("\"users\"")),
        "不得用别的方言的引号: {sqls:?}"
    );
}

/// 版本行的读写往返：写进去、读出来、删掉，并对 `applied_at` 的存储形态
/// 提出要求（RFC3339 UTC 文本，与实体的时间列同一套）。
#[tokio::test]
async fn version_rows_round_trip() {
    let db = mem_sqlite("migrate-version-roundtrip").await;
    version::ensure_version_table(&db).await.unwrap();
    assert!(version::read_applied(&db).await.unwrap().is_empty());
    version::record(&db, 7, "007_seven").await.unwrap();
    version::record(&db, 8, "008_eight").await.unwrap();
    let mut got = version::read_applied(&db).await.unwrap();
    got.sort_unstable();
    assert_eq!(got, vec![7, 8]);
    version::remove(&db, 7).await.unwrap();
    assert_eq!(version::read_applied(&db).await.unwrap(), vec![8]);
    let rows = db
        .query("SELECT applied_at FROM _ecat_migrations")
        .await
        .unwrap();
    let raw = rows[0]
        .get("applied_at")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).is_ok(),
        "applied_at 应是 RFC3339 文本，得到 {raw:?}"
    );
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分区 DDL：UTC 边界、标识符白名单、DROP 与恢复语句。
use std::sync::Mutex;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsError, Row, SqlExecutor};
use serde_json::Value;
use time::macros::datetime;

use super::{
    PartitionSpec, RestorePlan, RestoreSource, drop_partition, plan_range_partitions,
    restore_partition,
};
use crate::TieringError;

/// 记录执行的 SQL；`execute`/`execute_with` 都记，便于断言「拒绝时不执行」。
/// `query_with` 按 SQL 返回两处计数：staging 行数、目标分区是否存在。
struct Recorder {
    dialect: Dialect,
    sql: Mutex<Vec<String>>,
    loaded_rows: i64,
    target_partitions: i64,
}

impl Recorder {
    fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            sql: Mutex::new(Vec::new()),
            loaded_rows: 1,
            target_partitions: 1,
        }
    }

    fn with_loaded_rows(mut self, rows: i64) -> Self {
        self.loaded_rows = rows;
        self
    }

    fn with_target_partitions(mut self, partitions: i64) -> Self {
        self.target_partitions = partitions;
        self
    }

    fn statements(&self) -> Vec<String> {
        self.sql.lock().unwrap().clone()
    }
}

#[async_trait]
impl SqlExecutor for Recorder {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.sql.lock().unwrap().push(sql.to_owned());
        Ok(1)
    }

    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        Ok(Vec::new())
    }

    async fn query_with(&self, sql: &str, _params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        let n = if sql.contains("information_schema.PARTITIONS") {
            self.target_partitions
        } else {
            self.loaded_rows
        };
        Ok(vec![Row::new(vec!["n".into()], vec![serde_json::json!(n)])])
    }

    async fn execute_with(&self, sql: &str, _params: &[Value]) -> Result<u64, RdbmsError> {
        self.execute(sql).await
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

#[test]
fn partition_sql_uses_utc_boundaries_and_validated_identifiers() {
    let spec = PartitionSpec {
        table: "events".into(),
        column: "ts".into(),
        months: 1,
    };
    // +08:00 的输入也必须落成 UTC 边界。
    let from = datetime!(2026-01-01 08:00:00 +08:00);
    let to = datetime!(2026-03-01 00:00:00 UTC);
    assert_eq!(
        plan_range_partitions(&spec, from, to).unwrap(),
        "ALTER TABLE `events` PARTITION BY RANGE COLUMNS(`ts`) (\n  \
         PARTITION p202601 VALUES LESS THAN ('2026-02-01 00:00:00'),\n  \
         PARTITION p202602 VALUES LESS THAN ('2026-03-01 00:00:00'),\n  \
         PARTITION pmax VALUES LESS THAN (MAXVALUE)\n)"
    );
}

#[test]
fn partition_sql_steps_by_configured_months() {
    let spec = PartitionSpec {
        table: "events".into(),
        column: "ts".into(),
        months: 2,
    };
    let from = datetime!(2026-01-01 00:00:00 UTC);
    let to = datetime!(2026-05-01 00:00:00 UTC);
    let sql = plan_range_partitions(&spec, from, to).unwrap();
    assert!(
        sql.contains("PARTITION p202601 VALUES LESS THAN ('2026-03-01 00:00:00')"),
        "{sql}"
    );
    assert!(
        sql.contains("PARTITION p202603 VALUES LESS THAN ('2026-05-01 00:00:00')"),
        "{sql}"
    );
    assert_eq!(sql.matches("PARTITION p").count(), 3, "{sql}"); // 2 个区间 + pmax
}

#[test]
fn partition_sql_rejects_bad_ident() {
    let from = datetime!(2026-01-01 00:00:00 UTC);
    let to = datetime!(2026-02-01 00:00:00 UTC);
    for (table, column) in [("events; DROP TABLE users", "ts"), ("events", "ts`")] {
        let spec = PartitionSpec {
            table: table.into(),
            column: column.into(),
            months: 1,
        };
        assert!(
            matches!(
                plan_range_partitions(&spec, from, to),
                Err(TieringError::Config(_))
            ),
            "{table}/{column}"
        );
    }
    let zero = PartitionSpec {
        table: "events".into(),
        column: "ts".into(),
        months: 0,
    };
    assert!(matches!(
        plan_range_partitions(&zero, from, to),
        Err(TieringError::Config(_))
    ));
    let reversed = PartitionSpec {
        table: "events".into(),
        column: "ts".into(),
        months: 1,
    };
    assert!(matches!(
        plan_range_partitions(&reversed, to, from),
        Err(TieringError::Config(_))
    ));
}

#[tokio::test]
async fn drop_partition_rejects_semicolon() {
    let db = Recorder::new(Dialect::MySql);
    let err = drop_partition(&db, "events", "p1; DROP TABLE users")
        .await
        .unwrap_err();
    assert!(matches!(err, TieringError::Config(_)), "{err}");
    let err = drop_partition(&db, "events`; DROP TABLE users; --", "p1")
        .await
        .unwrap_err();
    assert!(matches!(err, TieringError::Config(_)), "{err}");
    assert!(db.statements().is_empty(), "拒绝时不得执行任何 SQL");

    assert_eq!(drop_partition(&db, "events", "p2026_01").await.unwrap(), 1);
    assert_eq!(
        db.statements(),
        ["ALTER TABLE `events` DROP PARTITION `p2026_01`"]
    );

    let sqlite = Recorder::new(Dialect::Sqlite);
    assert!(matches!(
        drop_partition(&sqlite, "events", "p1").await,
        Err(TieringError::UnsupportedDialect)
    ));
    assert!(sqlite.statements().is_empty());
}

fn restore_plan() -> RestorePlan {
    RestorePlan {
        table: "events".into(),
        partition: "p2026_01".into(),
        source: RestoreSource::Clickhouse,
    }
}

#[tokio::test]
async fn restore_uses_staging_and_exchange_partition() {
    let db = Recorder::new(Dialect::MySql);
    assert_eq!(restore_partition(&db, &restore_plan()).await.unwrap(), 1);
    let stmts = db.statements();
    assert_eq!(stmts.len(), 5, "{stmts:?}");
    // 残留 staging 会让 CREATE 失败，必须先删。
    assert_eq!(stmts[0], "DROP TABLE IF EXISTS `events_restore_p2026_01`");
    assert_eq!(
        stmts[1],
        "CREATE TABLE `events_restore_p2026_01` LIKE `events`"
    );
    // CREATE LIKE 会复制分区定义，必须先去掉，EXCHANGE 才能接受 staging。
    assert_eq!(
        stmts[2],
        "ALTER TABLE `events_restore_p2026_01` REMOVE PARTITIONING"
    );
    assert_eq!(
        stmts[3],
        "ALTER TABLE `events` EXCHANGE PARTITION `p2026_01` WITH TABLE `events_restore_p2026_01`"
    );
    assert!(
        stmts[4].starts_with("DELETE FROM tier_archive_jobs"),
        "恢复后必须清掉陈旧的归档状态：{stmts:?}"
    );
    let drop = stmts
        .iter()
        .position(|sql| sql.starts_with("DROP TABLE IF EXISTS"))
        .expect("missing DROP TABLE IF EXISTS");
    let create = stmts
        .iter()
        .position(|sql| sql.starts_with("CREATE TABLE"))
        .expect("missing CREATE TABLE");
    assert!(drop < create, "DROP IF EXISTS 必须先于 CREATE：{stmts:?}");

    let bad = RestorePlan {
        table: "events; DROP TABLE users".into(),
        partition: "p2026_01".into(),
        source: RestoreSource::S3,
    };
    let err = restore_partition(&db, &bad).await.unwrap_err();
    assert!(matches!(err, TieringError::Config(_)), "{err}");
    assert_eq!(db.statements().len(), 5, "拒绝时不得追加 SQL");
}

#[tokio::test]
async fn restore_rejects_empty_staging() {
    let db = Recorder::new(Dialect::MySql).with_loaded_rows(0);
    let err = restore_partition(&db, &restore_plan()).await.unwrap_err();
    assert!(matches!(err, TieringError::State(_)), "{err}");
    assert!(err.to_string().contains("empty"), "{err}");
    let stmts = db.statements();
    assert!(
        !stmts.iter().any(|sql| sql.contains("EXCHANGE")),
        "空 staging 不得 EXCHANGE：{stmts:?}"
    );
    assert!(
        !stmts.iter().any(|sql| sql.contains("DELETE")),
        "空 staging 不得重置状态：{stmts:?}"
    );
}

#[tokio::test]
async fn restore_rejects_missing_partition() {
    let db = Recorder::new(Dialect::MySql).with_target_partitions(0);
    let err = restore_partition(&db, &restore_plan()).await.unwrap_err();
    assert!(matches!(err, TieringError::State(_)), "{err}");
    assert!(err.to_string().contains("REORGANIZE"), "{err}");
    let stmts = db.statements();
    assert!(
        !stmts.iter().any(|sql| sql.contains("EXCHANGE")),
        "分区不存在不得 EXCHANGE：{stmts:?}"
    );
    assert!(
        !stmts.iter().any(|sql| sql.contains("DELETE")),
        "分区不存在不得重置状态：{stmts:?}"
    );
}

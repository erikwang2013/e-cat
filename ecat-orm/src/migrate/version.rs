// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `_ecat_migrations` 版本表的 DDL 与读写。
//!
//! 建表走的是与用户实体**同一套** [`super::create_table_sql`] —— 另写一份必然
//! 漂移，而漂移的表现是「版本表在某个方言上建不出来」。

use ecat_data::SqlExecutor;
use serde_json::{Value, json};

use super::create_table_sql;
use crate::dialect::lookup;
use crate::entity::{ColType, ColumnMeta, EntityFlags, EntityMeta};
use crate::error::OrmError;

/// 版本表名。下划线开头：它不是业务表，别混进实体清单里。
pub(crate) const TABLE: &str = "_ecat_migrations";

static COLS: [ColumnMeta; 3] = [
    ColumnMeta {
        name: "version",
        ty: ColType::I64,
        nullable: false,
        pk: true,
        auto_increment: false,
    },
    ColumnMeta {
        name: "name",
        ty: ColType::Text,
        nullable: false,
        pk: false,
        auto_increment: false,
    },
    ColumnMeta {
        name: "applied_at",
        ty: ColType::Timestamp,
        nullable: false,
        pk: false,
        auto_increment: false,
    },
];

static META: EntityMeta = EntityMeta {
    table: TABLE,
    pk: "version",
    columns: &COLS,
    relations: &[],
    flags: EntityFlags::NONE,
};

/// 确保版本表存在。
///
/// MSSQL 的建表前缀里**没有** `IF NOT EXISTS`，所以要按
/// [`DialectSpec::needs_exists_check_before_create`] 先查
/// `INFORMATION_SCHEMA.TABLES`；**表名走绑定参数**，不拼进 SQL。
pub(crate) async fn ensure_version_table<X: SqlExecutor + ?Sized>(db: &X) -> Result<(), OrmError> {
    let dialect = db.dialect();
    let spec = lookup(dialect);
    if spec.needs_exists_check_before_create() {
        let sql = format!(
            "SELECT 1 FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_NAME = {}",
            spec.placeholder(1)
        );
        let rows = db.query_with(&sql, &[Value::String(TABLE.into())]).await?;
        if !rows.is_empty() {
            return Ok(());
        }
    }
    db.execute(&create_table_sql(&META, dialect)).await?;
    Ok(())
}

/// 版本表里已应用的版本号。**未排序** —— 对比与排序由 [`super::classify`] 做。
pub(crate) async fn read_applied<X: SqlExecutor + ?Sized>(db: &X) -> Result<Vec<i64>, OrmError> {
    let spec = lookup(db.dialect());
    let sql = format!(
        "SELECT {} FROM {}",
        spec.quote("version"),
        spec.quote(TABLE)
    );
    let rows = db.query(&sql).await?;
    rows.iter()
        .map(|r| crate::value::from_row_col::<i64>(r, "version"))
        .collect()
}

/// 记一行「已应用」。时间存 RFC3339 UTC 文本（与实体的时间列同一套）。
pub(crate) async fn record<X: SqlExecutor + ?Sized>(
    db: &X,
    version: i64,
    name: &str,
) -> Result<(), OrmError> {
    let spec = lookup(db.dialect());
    let cols = ["version", "name", "applied_at"]
        .map(|c| spec.quote(c))
        .join(", ");
    let ph = (1..=COLS.len())
        .map(|i| spec.placeholder(i))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("INSERT INTO {} ({cols}) VALUES ({ph})", spec.quote(TABLE));
    let applied_at = crate::value::ColumnValue::to_json(&crate::crud::now());
    db.execute_with(&sql, &[json!(version), json!(name), applied_at])
        .await?;
    Ok(())
}

/// 删掉一行（`down` 用）—— 与 [`record`] 互逆。
pub(crate) async fn remove<X: SqlExecutor + ?Sized>(db: &X, version: i64) -> Result<(), OrmError> {
    let spec = lookup(db.dialect());
    let sql = format!(
        "DELETE FROM {} WHERE {} = {}",
        spec.quote(TABLE),
        spec.quote("version"),
        spec.placeholder(1)
    );
    db.execute_with(&sql, &[json!(version)]).await?;
    Ok(())
}

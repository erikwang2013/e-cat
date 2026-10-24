// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分区 DDL 助手：`RANGE COLUMNS` 计划、`DROP PARTITION` 与恢复。
//!
//! 所有标识符先过 [`validate_ident`]，SQL 是固定模板 —— 不接受调用方 SQL。
use ecat_data::{Dialect, SqlExecutor};
use time::macros::format_description;
use time::util::days_in_month;
use time::{Date, Month, OffsetDateTime, UtcOffset};

use crate::state;
use crate::{TieringError, validate_ident};

/// 一张表的 `RANGE COLUMNS` 分区计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionSpec {
    pub table: String,
    pub column: String,
    pub months: u32,
}

/// 恢复计划：把冷层数据装进 staging 表后与原表交换分区。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub table: String,
    pub partition: String,
    pub source: RestoreSource,
}

/// 恢复数据来自哪个冷层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreSource {
    Clickhouse,
    S3,
}

/// 生成覆盖 `[from, to)` 的 `ALTER TABLE ... PARTITION BY RANGE COLUMNS` 语句。
///
/// 边界一律换算成 UTC；每个分区以起始时刻的 UTC 年月命名，末段由 `pmax` 兜底。
/// 执行 DDL 的会话对 `TIMESTAMP` 列也需 `SET time_zone = '+00:00'`，边界字面量才按 UTC 解释。
pub fn plan_range_partitions(
    spec: &PartitionSpec,
    from: OffsetDateTime,
    to: OffsetDateTime,
) -> Result<String, TieringError> {
    validate_ident(&spec.table)?;
    validate_ident(&spec.column)?;
    if spec.months == 0 {
        return Err(TieringError::Config("partition months must be > 0".into()));
    }
    let from = from.to_offset(UtcOffset::UTC);
    let to = to.to_offset(UtcOffset::UTC);
    if to <= from {
        return Err(TieringError::Config(
            "partition range end must be after start".into(),
        ));
    }
    let mut parts = Vec::new();
    let mut start = from;
    while start < to {
        let end = add_months(start, spec.months)?.min(to);
        parts.push(format!(
            "PARTITION {} VALUES LESS THAN ('{}')",
            partition_name(start)?,
            fmt_utc(end)?
        ));
        start = end;
    }
    parts.push("PARTITION pmax VALUES LESS THAN (MAXVALUE)".to_owned());
    let body = parts
        .iter()
        .map(|part| format!("  {part}"))
        .collect::<Vec<_>>()
        .join(",\n");
    Ok(format!(
        "ALTER TABLE `{}` PARTITION BY RANGE COLUMNS(`{}`) (\n{body}\n)",
        spec.table, spec.column
    ))
}

/// 删除一个分区；表名/分区名先过白名单，再拼进固定模板。
pub async fn drop_partition(
    db: &dyn SqlExecutor,
    table: &str,
    partition: &str,
) -> Result<u64, TieringError> {
    validate_ident(table)?;
    validate_ident(partition)?;
    ensure_mysql(db)?;
    let rows = db
        .execute(&format!(
            "ALTER TABLE `{table}` DROP PARTITION `{partition}`"
        ))
        .await?;
    Ok(rows)
}

/// 用 staging 表恢复分区，然后清掉该分区的归档状态。
pub async fn restore_partition(
    db: &dyn SqlExecutor,
    plan: &RestorePlan,
) -> Result<u64, TieringError> {
    validate_ident(&plan.table)?;
    validate_ident(&plan.partition)?;
    ensure_mysql(db)?;
    // shortcut: staging 名是「表_restore_分区」，两者合计超 64 字符会撞 MySQL 标识符上限；
    // 超长表名需改成短名映射。
    let staging = format!("{}_restore_{}", plan.table, plan.partition);
    validate_ident(&staging)?;
    db.execute(&format!("CREATE TABLE `{staging}` LIKE `{}`", plan.table))
        .await?;
    let rows = db
        .execute(&format!(
            "ALTER TABLE `{}` EXCHANGE PARTITION `{}` WITH TABLE `{staging}`",
            plan.table, plan.partition
        ))
        .await?;
    // 恢复的数据又进了热表：陈旧的 Verified 行会让下一次归档直接 DROP 掉它。
    state::reset_partition(db, &plan.table, &plan.partition).await?;
    Ok(rows)
}

/// `RANGE COLUMNS` 的 `CREATE TABLE LIKE` / `EXCHANGE PARTITION` 只有 MySQL 语法。
fn ensure_mysql(db: &dyn SqlExecutor) -> Result<(), TieringError> {
    match db.dialect() {
        Dialect::MySql => Ok(()),
        _ => Err(TieringError::UnsupportedDialect),
    }
}

fn partition_name(start: OffsetDateTime) -> Result<String, TieringError> {
    let (year, month, _) = start.date().to_calendar_date();
    let name = format!("p{year:04}{:02}", u8::from(month));
    validate_ident(&name)?;
    Ok(name)
}

/// 加自然月并保留时刻；目标月没有该日时取当月最后一天。
fn add_months(ts: OffsetDateTime, months: u32) -> Result<OffsetDateTime, TieringError> {
    let ts = ts.to_offset(UtcOffset::UTC);
    let (year, month, day) = ts.date().to_calendar_date();
    let total = i64::from(year) * 12 + i64::from(u8::from(month) - 1) + i64::from(months);
    let year = i32::try_from(total.div_euclid(12)).map_err(|_| {
        TieringError::Config(format!("partition boundary year out of range: {total}"))
    })?;
    let month = u8::try_from(total.rem_euclid(12) + 1)
        .map_err(|_| TieringError::Config("partition boundary month out of range".into()))?;
    let month = Month::try_from(month)
        .map_err(|_| TieringError::Config("partition boundary month out of range".into()))?;
    let date = Date::from_calendar_date(year, month, day.min(days_in_month(month, year)))
        .map_err(|e| TieringError::Config(format!("invalid partition boundary: {e}")))?;
    Ok(ts.replace_date(date))
}

fn fmt_utc(ts: OffsetDateTime) -> Result<String, TieringError> {
    ts.to_offset(UtcOffset::UTC)
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second]"
        ))
        .map_err(|e| TieringError::Config(format!("failed to format partition boundary: {e}")))
}

#[cfg(test)]
mod tests;

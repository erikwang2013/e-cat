// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 归档任务状态表：chunk 级阶段机与幂等 upsert。
use std::sync::Arc;

use ecat_data::{Dialect, Row, SqlExecutor};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::TieringError;

const TABLE: &str = "tier_archive_jobs";

/// 归档 chunk 的阶段；只允许 `Pending → Copied → Verified → Dropped`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivePhase {
    Pending,
    Copied,
    Verified,
    Dropped,
}

impl ArchivePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Copied => "copied",
            Self::Verified => "verified",
            Self::Dropped => "dropped",
        }
    }

    fn parse(s: &str) -> Result<Self, TieringError> {
        match s {
            "pending" => Ok(Self::Pending),
            "copied" => Ok(Self::Copied),
            "verified" => Ok(Self::Verified),
            "dropped" => Ok(Self::Dropped),
            other => Err(TieringError::State(format!(
                "unknown archive phase `{other}`"
            ))),
        }
    }

    /// 允许迁入 `self` 的唯一来源阶段；`Pending` 无来源、`Dropped` 无后继。
    fn predecessor(self) -> Option<Self> {
        match self {
            Self::Pending => None,
            Self::Copied => Some(Self::Pending),
            Self::Verified => Some(Self::Copied),
            Self::Dropped => Some(Self::Verified),
        }
    }
}

/// 状态表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveJobState {
    pub table: String,
    pub partition: String,
    pub chunk: u64,
    pub low_id: i64,
    pub high_id: i64,
    pub rows: u64,
    pub checksum: Option<String>,
    pub phase: ArchivePhase,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone)]
pub struct ArchiveStateStore {
    db: Arc<dyn SqlExecutor>,
}

impl ArchiveStateStore {
    pub fn new(db: Arc<dyn SqlExecutor>) -> Self {
        Self { db }
    }

    /// 建状态表；MSSQL 本期不支持，与其他方法一样返回 `UnsupportedDialect`。
    pub async fn ensure_schema(&self) -> Result<(), TieringError> {
        let dialect = self.dialect()?;
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {TABLE} (
                table_name VARCHAR(255) NOT NULL,
                partition_name VARCHAR(255) NOT NULL,
                chunk BIGINT NOT NULL,
                low_id BIGINT NOT NULL,
                high_id BIGINT NOT NULL,
                {rows} BIGINT NOT NULL,
                checksum VARCHAR(255),
                phase VARCHAR(16) NOT NULL,
                updated_at VARCHAR(64) NOT NULL,
                PRIMARY KEY (table_name, partition_name, chunk)
            )",
            rows = rows_col(dialect)
        );
        self.db.execute(&sql).await?;
        Ok(())
    }

    /// 按主键整行写入（幂等）：已存在则覆盖，不做状态机校验。
    pub async fn upsert(&self, s: &ArchiveJobState) -> Result<(), TieringError> {
        let dialect = self.dialect()?;
        let rows = rows_col(dialect);
        let sql = match dialect {
            Dialect::MySql => format!(
                "INSERT INTO {TABLE} ({cols}) VALUES ({ph}) ON DUPLICATE KEY UPDATE \
                 low_id = VALUES(low_id), high_id = VALUES(high_id), \
                 {rows} = VALUES({rows}), checksum = VALUES(checksum), \
                 phase = VALUES(phase), updated_at = VALUES(updated_at)",
                cols = columns(dialect),
                ph = placeholders(dialect, 9),
            ),
            _ => format!(
                "INSERT INTO {TABLE} ({cols}) VALUES ({ph}) \
                 ON CONFLICT (table_name, partition_name, chunk) DO UPDATE SET \
                 low_id = excluded.low_id, high_id = excluded.high_id, \
                 {rows} = excluded.{rows}, checksum = excluded.checksum, \
                 phase = excluded.phase, updated_at = excluded.updated_at",
                cols = columns(dialect),
                ph = placeholders(dialect, 9),
            ),
        };
        let params = [
            Value::String(s.table.clone()),
            Value::String(s.partition.clone()),
            Value::Number(
                i64::try_from(s.chunk)
                    .map_err(|_| out_of_range("chunk"))?
                    .into(),
            ),
            Value::Number(s.low_id.into()),
            Value::Number(s.high_id.into()),
            Value::Number(
                i64::try_from(s.rows)
                    .map_err(|_| out_of_range("rows"))?
                    .into(),
            ),
            s.checksum.clone().map_or(Value::Null, Value::String),
            Value::String(s.phase.as_str().into()),
            Value::String(fmt_ts(s.updated_at)?),
        ];
        self.db.execute_with(&sql, &params).await?;
        Ok(())
    }

    /// 表的最新状态更新时间；没有状态行时为 `None`。
    pub async fn latest_updated_at(
        &self,
        table: &str,
    ) -> Result<Option<OffsetDateTime>, TieringError> {
        let dialect = self.dialect()?;
        let sql = format!(
            "SELECT MAX(updated_at) AS latest FROM {TABLE} WHERE table_name = {}",
            placeholder(dialect, 1)
        );
        let rows = self
            .db
            .query_with(&sql, &[Value::String(table.into())])
            .await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        match row.get("latest") {
            Some(Value::Null) | None => Ok(None),
            Some(Value::String(text)) => OffsetDateTime::parse(text, &Rfc3339)
                .map(Some)
                .map_err(|e| TieringError::State(format!("latest updated_at is not RFC3339: {e}"))),
            _ => Err(bad_column("latest")),
        }
    }

    /// 列出状态行；`table` 为 `None` 时列出全部表。
    pub async fn list(&self, table: Option<&str>) -> Result<Vec<ArchiveJobState>, TieringError> {
        let dialect = self.dialect()?;
        let (sql, params) = match table {
            Some(table) => (
                format!(
                    "SELECT {} FROM {} WHERE table_name = {} ORDER BY partition_name, chunk",
                    columns(dialect),
                    TABLE,
                    placeholder(dialect, 1)
                ),
                vec![Value::String(table.into())],
            ),
            None => (
                format!(
                    "SELECT {} FROM {} ORDER BY table_name, partition_name, chunk",
                    columns(dialect),
                    TABLE
                ),
                Vec::new(),
            ),
        };
        let rows = if params.is_empty() {
            self.db.query(&sql).await?
        } else {
            self.db.query_with(&sql, &params).await?
        };
        rows.iter().map(Self::from_row).collect()
    }

    pub async fn load(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
    ) -> Result<Option<ArchiveJobState>, TieringError> {
        let dialect = self.dialect()?;
        let sql = format!(
            "SELECT {cols} FROM {TABLE} \
             WHERE table_name = {p1} AND partition_name = {p2} AND chunk = {p3}",
            cols = columns(dialect),
            p1 = placeholder(dialect, 1),
            p2 = placeholder(dialect, 2),
            p3 = placeholder(dialect, 3),
        );
        let params = [
            Value::String(table.into()),
            Value::String(partition.into()),
            Value::Number(
                i64::try_from(chunk)
                    .map_err(|_| out_of_range("chunk"))?
                    .into(),
            ),
        ];
        let rows = self.db.query_with(&sql, &params).await?;
        rows.first().map(Self::from_row).transpose()
    }

    /// 单步推进阶段；`Pending → Copied → Verified → Dropped` 之外一律报错。
    ///
    /// `UPDATE` 带「当前阶段 = 合法前驱」条件，比较与写入是同一条语句，
    /// 两个进程同时推进时只有一个能成功。
    pub async fn advance(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        to: ArchivePhase,
        checksum: Option<&str>,
    ) -> Result<(), TieringError> {
        let dialect = self.dialect()?;
        let from = to.predecessor().ok_or_else(|| {
            TieringError::State(format!(
                "archive phase `{}` has no legal predecessor",
                to.as_str()
            ))
        })?;
        let sql = format!(
            "UPDATE {TABLE} SET phase = {p1}, checksum = COALESCE({p2}, checksum), \
             updated_at = {p3} \
             WHERE table_name = {p4} AND partition_name = {p5} AND chunk = {p6} \
             AND phase = {p7}",
            p1 = placeholder(dialect, 1),
            p2 = placeholder(dialect, 2),
            p3 = placeholder(dialect, 3),
            p4 = placeholder(dialect, 4),
            p5 = placeholder(dialect, 5),
            p6 = placeholder(dialect, 6),
            p7 = placeholder(dialect, 7),
        );
        let params = [
            Value::String(to.as_str().into()),
            checksum.map_or(Value::Null, |c| Value::String(c.into())),
            Value::String(fmt_ts(OffsetDateTime::now_utc())?),
            Value::String(table.into()),
            Value::String(partition.into()),
            Value::Number(
                i64::try_from(chunk)
                    .map_err(|_| out_of_range("chunk"))?
                    .into(),
            ),
            Value::String(from.as_str().into()),
        ];
        if self.db.execute_with(&sql, &params).await? > 0 {
            return Ok(());
        }
        match self.load(table, partition, chunk).await? {
            None => Err(TieringError::State(format!(
                "archive job {table}/{partition}/{chunk} not found"
            ))),
            Some(current) => Err(TieringError::State(format!(
                "invalid archive phase transition {} -> {} for {table}/{partition}/{chunk}",
                current.phase.as_str(),
                to.as_str()
            ))),
        }
    }

    /// 清掉某表/某分区的全部 chunk 状态；恢复后调用，避免陈旧的 `Verified`
    /// 行让下一次归档再次 `DROP PARTITION`。
    pub async fn reset_partition(&self, table: &str, partition: &str) -> Result<u64, TieringError> {
        reset_partition(&*self.db, table, partition).await
    }

    fn dialect(&self) -> Result<Dialect, TieringError> {
        supported_dialect(&*self.db)
    }

    fn from_row(row: &Row) -> Result<ArchiveJobState, TieringError> {
        Ok(ArchiveJobState {
            table: text(row, "table_name")?.to_owned(),
            partition: text(row, "partition_name")?.to_owned(),
            chunk: uint(row, "chunk")?,
            low_id: int(row, "low_id")?,
            high_id: int(row, "high_id")?,
            rows: uint(row, "rows")?,
            checksum: match row.get("checksum") {
                Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                _ => return Err(bad_column("checksum")),
            },
            phase: ArchivePhase::parse(text(row, "phase")?)?,
            updated_at: OffsetDateTime::parse(text(row, "updated_at")?, &Rfc3339)
                .map_err(|e| TieringError::State(format!("updated_at is not RFC3339: {e}")))?,
        })
    }
}

/// 同 [`ArchiveStateStore::reset_partition`]，供只有 `&dyn SqlExecutor` 的恢复路径使用。
pub(crate) async fn reset_partition(
    db: &dyn SqlExecutor,
    table: &str,
    partition: &str,
) -> Result<u64, TieringError> {
    let dialect = supported_dialect(db)?;
    let sql = format!(
        "DELETE FROM {TABLE} WHERE table_name = {p1} AND partition_name = {p2}",
        p1 = placeholder(dialect, 1),
        p2 = placeholder(dialect, 2),
    );
    let params = [Value::String(table.into()), Value::String(partition.into())];
    let rows = db.execute_with(&sql, &params).await?;
    Ok(rows)
}

fn supported_dialect(db: &dyn SqlExecutor) -> Result<Dialect, TieringError> {
    match db.dialect() {
        Dialect::Mssql => Err(TieringError::UnsupportedDialect),
        dialect => Ok(dialect),
    }
}

/// `rows` 是 MySQL 保留字：MySQL 用反引号，其余方言用 ANSI 双引号。
fn rows_col(dialect: Dialect) -> &'static str {
    if dialect == Dialect::MySql {
        "`rows`"
    } else {
        "\"rows\""
    }
}

fn columns(dialect: Dialect) -> String {
    format!(
        "table_name, partition_name, chunk, low_id, high_id, {}, checksum, phase, updated_at",
        rows_col(dialect)
    )
}

/// Postgres 用 `$n`，其余方言用 `?`。
fn placeholder(dialect: Dialect, n: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${n}"),
        _ => "?".to_string(),
    }
}

fn placeholders(dialect: Dialect, count: usize) -> String {
    (1..=count)
        .map(|n| placeholder(dialect, n))
        .collect::<Vec<_>>()
        .join(", ")
}

fn fmt_ts(ts: OffsetDateTime) -> Result<String, TieringError> {
    ts.to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|e| TieringError::State(format!("failed to format updated_at: {e}")))
}

fn bad_column(col: &str) -> TieringError {
    TieringError::State(format!(
        "archive state column `{col}` missing or of unexpected type"
    ))
}

fn out_of_range(col: &str) -> TieringError {
    TieringError::State(format!("archive state `{col}` exceeds i64 range"))
}

fn int(row: &Row, col: &str) -> Result<i64, TieringError> {
    row.get(col)
        .and_then(Value::as_i64)
        .ok_or_else(|| bad_column(col))
}

fn text<'a>(row: &'a Row, col: &str) -> Result<&'a str, TieringError> {
    row.get(col)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_column(col))
}

fn uint(row: &Row, col: &str) -> Result<u64, TieringError> {
    u64::try_from(int(row, col)?)
        .map_err(|_| TieringError::State(format!("archive state column `{col}` is negative")))
}

#[cfg(test)]
mod tests;

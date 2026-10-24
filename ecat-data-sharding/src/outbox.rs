// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Outbox 存储：租约 claim、幂等完成与退避重试。
use std::sync::Arc;

use base64::Engine as _;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::ShardError;

#[cfg(test)]
mod tests;

const TABLE: &str = "ecat_outbox";
const COLS: &str = "id, aggregate, payload, status, attempts, next_attempt_at";

/// 固定宽度的 UTC 时间戳：SQL 里按字符串比较先后，宽度必须恒定。
const TS_FORMAT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z");

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

impl From<RdbmsError> for ShardError {
    fn from(e: RdbmsError) -> Self {
        Self::Database(e.to_string())
    }
}

/// outbox 记录状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    Pending,
    Processing,
    Done,
    Failed,
}

impl OutboxStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self, ShardError> {
        match s {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => Err(ShardError::Database(format!(
                "unknown outbox status `{other}`"
            ))),
        }
    }
}

/// outbox 表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    pub id: String,
    pub aggregate: String,
    pub payload: Vec<u8>,
    pub status: OutboxStatus,
    pub attempts: u32,
    pub next_attempt_at: OffsetDateTime,
}

/// 租约式 outbox 存储。
pub struct OutboxStore {
    db: Arc<dyn RdbmsClient>,
}

impl OutboxStore {
    pub fn new(db: Arc<dyn RdbmsClient>) -> Self {
        Self { db }
    }

    pub async fn ensure_schema(&self) -> Result<(), ShardError> {
        let sql = match self.dialect()? {
            // MySQL 的 TEXT/BLOB 不能直接做 PRIMARY KEY，主键列用 VARCHAR。
            Dialect::MySql => format!(
                "CREATE TABLE IF NOT EXISTS {TABLE} (
                    id VARCHAR(255) PRIMARY KEY,
                    aggregate VARCHAR(255) NOT NULL,
                    payload BLOB NOT NULL,
                    status VARCHAR(16) NOT NULL,
                    attempts INT NOT NULL,
                    next_attempt_at VARCHAR(64) NOT NULL,
                    claimed_at VARCHAR(64)
                )"
            ),
            Dialect::Postgres => format!(
                "CREATE TABLE IF NOT EXISTS {TABLE} (
                    id TEXT PRIMARY KEY,
                    aggregate TEXT NOT NULL,
                    payload BYTEA NOT NULL,
                    status TEXT NOT NULL,
                    attempts BIGINT NOT NULL,
                    next_attempt_at TEXT NOT NULL,
                    claimed_at TEXT
                )"
            ),
            _ => format!(
                "CREATE TABLE IF NOT EXISTS {TABLE} (
                    id TEXT PRIMARY KEY,
                    aggregate TEXT NOT NULL,
                    payload BLOB NOT NULL,
                    status TEXT NOT NULL,
                    attempts INTEGER NOT NULL,
                    next_attempt_at TEXT NOT NULL,
                    claimed_at TEXT
                )"
            ),
        };
        self.db.execute(&sql).await?;
        Ok(())
    }

    /// 插入一条记录；`id` 冲突返回数据库错误（去重由消费方的幂等 handler 负责）。
    pub async fn append(&self, r: &OutboxRecord) -> Result<(), ShardError> {
        let dialect = self.dialect()?;
        // payload 列是 BLOB/BYTEA：PG/MySQL 用各自的解码函数把 base64 参数落成字节。
        // SQLite 的 BLOB 亲和性不转换文本，存的是 base64 文本；读回时 `Row` 对真实
        // BLOB 也返回 base64，两条路径在 `from_row` 里统一解码。
        let payload = match dialect {
            Dialect::Postgres => format!("decode({}, 'base64')", placeholder(dialect, 3)),
            Dialect::MySql => format!("FROM_BASE64({})", placeholder(dialect, 3)),
            _ => placeholder(dialect, 3),
        };
        let sql = format!(
            "INSERT INTO {TABLE} (id, aggregate, payload, status, attempts, next_attempt_at, claimed_at) \
             VALUES ({}, {}, {payload}, {}, {}, {}, NULL)",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
            placeholder(dialect, 4),
            placeholder(dialect, 5),
            placeholder(dialect, 6),
        );
        let params = [
            Value::String(r.id.clone()),
            Value::String(r.aggregate.clone()),
            Value::String(B64.encode(&r.payload)),
            Value::String(r.status.as_str().into()),
            Value::Number(i64::from(r.attempts).into()),
            Value::String(fmt_ts(r.next_attempt_at)?),
        ];
        self.db.execute_with(&sql, &params).await?;
        Ok(())
    }

    /// 领取最多 `limit` 条到期记录并写入租约，返回的记录状态为 `Processing`。
    ///
    /// 先在事务里回收超过 `lease` 的 `Processing`（置回 `Pending`），再选取
    /// `next_attempt_at <= now` 的 `Pending`/`Failed` 行，最后按主键集合更新为
    /// `Processing`。SQLite 不支持 `UPDATE ... LIMIT`，故用主键集合更新。
    ///
    /// PG/MySQL 的选取带 `FOR UPDATE SKIP LOCKED`，并发 claim 不会双领；
    /// SQLite 由事务的写锁（回收 UPDATE 先行）与单写者串行化。
    pub async fn claim(
        &self,
        limit: u32,
        lease: Duration,
    ) -> Result<Vec<OutboxRecord>, ShardError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let dialect = self.dialect()?;
        let now = OffsetDateTime::now_utc();
        let cutoff = now
            .checked_sub(lease)
            .ok_or_else(|| ShardError::Config("outbox lease out of range".into()))?;
        let tx = self.db.transaction().await?;

        let reclaim = format!(
            "UPDATE {TABLE} SET status = 'pending', claimed_at = NULL \
             WHERE status = 'processing' AND claimed_at < {}",
            placeholder(dialect, 1),
        );
        tx.execute_with(&reclaim, &[Value::String(fmt_ts(cutoff)?)])
            .await?;

        let locks = match dialect {
            Dialect::Postgres | Dialect::MySql => " FOR UPDATE SKIP LOCKED",
            _ => "",
        };
        let select = format!(
            "SELECT {COLS} FROM {TABLE} \
             WHERE status IN ('pending', 'failed') AND next_attempt_at <= {} \
             ORDER BY next_attempt_at, id LIMIT {}{locks}",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
        );
        let rows = tx
            .query_with(
                &select,
                &[
                    Value::String(fmt_ts(now)?),
                    Value::Number(i64::from(limit).into()),
                ],
            )
            .await?;
        let records = rows
            .iter()
            .map(Self::from_row)
            .collect::<Result<Vec<_>, ShardError>>()?;
        if records.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let mut params = Vec::with_capacity(records.len() + 1);
        params.push(Value::String(fmt_ts(now)?));
        let id_placeholders = records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                params.push(Value::String(record.id.clone()));
                placeholder(dialect, index + 2)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let update = format!(
            "UPDATE {TABLE} SET status = 'processing', claimed_at = {} \
             WHERE id IN ({id_placeholders})",
            placeholder(dialect, 1),
        );
        tx.execute_with(&update, &params).await?;
        tx.commit().await?;

        Ok(records
            .into_iter()
            .map(|record| OutboxRecord {
                status: OutboxStatus::Processing,
                ..record
            })
            .collect())
    }

    /// 标记完成；重复调用是幂等的。
    pub async fn mark_done(&self, id: &str) -> Result<(), ShardError> {
        let dialect = self.dialect()?;
        let sql = format!(
            "UPDATE {TABLE} SET status = 'done' WHERE id = {}",
            placeholder(dialect, 1),
        );
        self.db
            .execute_with(&sql, &[Value::String(id.into())])
            .await?;
        Ok(())
    }

    /// 记录一次失败：`attempts + 1`，`next_attempt_at = now + retry_after`。
    /// 到点后 `claim` 会重新领取 `Failed` 行（退避重试）。
    pub async fn mark_failed(&self, id: &str, retry_after: Duration) -> Result<(), ShardError> {
        let dialect = self.dialect()?;
        let next = OffsetDateTime::now_utc()
            .checked_add(retry_after)
            .ok_or_else(|| ShardError::Config("outbox retry_after out of range".into()))?;
        let sql = format!(
            "UPDATE {TABLE} SET status = 'failed', attempts = attempts + 1, \
             next_attempt_at = {}, claimed_at = NULL WHERE id = {}",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
        );
        self.db
            .execute_with(
                &sql,
                &[Value::String(fmt_ts(next)?), Value::String(id.into())],
            )
            .await?;
        Ok(())
    }

    /// 未完成条数（积压）：`Pending`/`Processing`/`Failed` 都算，供健康检查用。
    pub async fn pending_count(&self) -> Result<u64, ShardError> {
        let rows = self
            .db
            .query(&format!(
                "SELECT COUNT(*) AS n FROM {TABLE} WHERE status <> 'done'"
            ))
            .await?;
        rows.first()
            .and_then(|row| row.get("n"))
            .and_then(Value::as_i64)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| ShardError::Database("outbox count returned no row".into()))
    }

    fn from_row(row: &Row) -> Result<OutboxRecord, ShardError> {
        Ok(OutboxRecord {
            id: text(row, "id")?.to_owned(),
            aggregate: text(row, "aggregate")?.to_owned(),
            payload: B64
                .decode(text(row, "payload")?)
                .map_err(|e| ShardError::Database(format!("outbox payload is not base64: {e}")))?,
            status: OutboxStatus::parse(text(row, "status")?)?,
            attempts: u32::try_from(int(row, "attempts")?)
                .map_err(|_| ShardError::Database("outbox attempts out of range".into()))?,
            next_attempt_at: parse_ts(text(row, "next_attempt_at")?)?,
        })
    }

    fn dialect(&self) -> Result<Dialect, ShardError> {
        match self.db.dialect() {
            Dialect::Mssql => Err(ShardError::UnsupportedDialect),
            dialect => Ok(dialect),
        }
    }
}

/// Postgres 用 `$n`，其余方言用 `?`。
fn placeholder(dialect: Dialect, n: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${n}"),
        _ => "?".into(),
    }
}

/// 固定宽度 UTC 时间戳，供 SQL 字符串比较。
fn fmt_ts(ts: OffsetDateTime) -> Result<String, ShardError> {
    ts.to_offset(UtcOffset::UTC)
        .format(TS_FORMAT)
        .map_err(|e| ShardError::Database(format!("failed to format outbox timestamp: {e}")))
}

fn parse_ts(text: &str) -> Result<OffsetDateTime, ShardError> {
    OffsetDateTime::parse(text, &Rfc3339)
        .map_err(|e| ShardError::Database(format!("outbox timestamp is not RFC3339: {e}")))
}

fn text<'a>(row: &'a Row, col: &str) -> Result<&'a str, ShardError> {
    row.get(col)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_column(col))
}

fn int(row: &Row, col: &str) -> Result<i64, ShardError> {
    row.get(col)
        .and_then(Value::as_i64)
        .ok_or_else(|| bad_column(col))
}

fn bad_column(col: &str) -> ShardError {
    ShardError::Database(format!(
        "outbox column `{col}` missing or of unexpected type"
    ))
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分片迁移 / 扩缩容执行器（A 阶段，feature `migrate`）。
//!
//! 流程：`ShardPlan::diff` 生成搬移计划 → `backfill` 幂等回填 →
//! `verify` 行数 + XOR checksum 核对 → `cutover` reload 新拓扑。
//! 旧分片保留，`rollback` 只切回旧拓扑（不反向搬数据）。
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ecat_data::{Dialect, RdbmsClient, Row};
use ecat_lock::{FenceGuard, FencedLock, LockError};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::catalog::{ShardCatalog, ShardError, ShardTable};

mod backfill;
mod cutover;
mod plan;
mod verify;

#[cfg(test)]
mod tests;

pub use plan::{KeyRange, ShardMove, ShardPlan};

const JOB_TABLE: &str = "shard_migration_jobs";
const DEFAULT_CHUNK_SIZE: usize = 1000;

/// 一次 backfill 的汇总；`verified` 恒为 false（校验由 [`ShardMigrator::verify`] 完成）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub moved_rows: u64,
    pub chunks: u64,
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Backfilling,
    Verified,
    Cutover,
    Done,
    RolledBack,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Backfilling => "backfilling",
            Self::Verified => "verified",
            Self::Cutover => "cutover",
            Self::Done => "done",
            Self::RolledBack => "rolled_back",
        }
    }

    fn parse(text: &str) -> Result<Self, ShardError> {
        match text {
            "pending" => Ok(Self::Pending),
            "backfilling" => Ok(Self::Backfilling),
            "verified" => Ok(Self::Verified),
            "cutover" => Ok(Self::Cutover),
            "done" => Ok(Self::Done),
            "rolled_back" => Ok(Self::RolledBack),
            other => Err(ShardError::Database(format!(
                "unknown migration phase `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
struct JobState {
    phase: Phase,
    cursor: Option<String>,
    rows: u64,
    checksum: u64,
}

struct FenceSetting {
    lock: Arc<dyn FencedLock>,
    key: String,
    ttl: Duration,
}

/// 分片搬移执行器；状态存 `shard_migration_jobs`。
pub struct ShardMigrator {
    plan: ShardPlan,
    clients: HashMap<String, Arc<dyn RdbmsClient>>,
    state: Arc<dyn RdbmsClient>,
    old_catalog: ShardCatalog,
    new_catalog: ShardCatalog,
    chunk_size: usize,
    fence: Option<FenceSetting>,
}

impl ShardMigrator {
    /// 校验两个目录与计划中的表/分片/客户端，然后构造执行器。
    pub fn new(
        plan: ShardPlan,
        clients: HashMap<String, Arc<dyn RdbmsClient>>,
        state: Arc<dyn RdbmsClient>,
        old_catalog: ShardCatalog,
        new_catalog: ShardCatalog,
    ) -> Result<Self, ShardError> {
        old_catalog.validate()?;
        new_catalog.validate()?;
        for mv in &plan.moves {
            let old_table = old_catalog.table(&mv.table)?;
            let new_table = new_catalog.table(&mv.table)?;
            if old_table.key != new_table.key || old_table.pk != new_table.pk {
                return Err(ShardError::Config(format!(
                    "table `{}` changes key/pk; migration is unsupported",
                    mv.table
                )));
            }
            if !old_table.shards.iter().any(|s| s.name == mv.from) {
                return Err(ShardError::Config(format!(
                    "move source shard `{}` is not in the old catalog",
                    mv.from
                )));
            }
            if !new_table.shards.iter().any(|s| s.name == mv.to) {
                return Err(ShardError::Config(format!(
                    "move target shard `{}` is not in the new catalog",
                    mv.to
                )));
            }
            for shard in [&mv.from, &mv.to] {
                if !clients.contains_key(shard) {
                    return Err(ShardError::Config(format!(
                        "move references shard `{shard}` without a client"
                    )));
                }
            }
        }
        Ok(Self {
            plan,
            clients,
            state,
            old_catalog,
            new_catalog,
            chunk_size: DEFAULT_CHUNK_SIZE,
            fence: None,
        })
    }

    /// 单批行数；0 会被钳到 1。
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    /// 单执行者 fencing：backfill 持有 `key`，每批 renew + 校验 fence。
    pub fn with_fence(
        mut self,
        lock: Arc<dyn FencedLock>,
        key: impl Into<String>,
        ttl: Duration,
    ) -> Self {
        self.fence = Some(FenceSetting {
            lock,
            key: key.into(),
            ttl,
        });
        self
    }

    /// 建状态表（幂等）；`backfill` 会自动调用。
    pub async fn ensure_schema(&self) -> Result<(), ShardError> {
        let dialect = self.state_dialect()?;
        let id_type = if dialect == Dialect::MySql {
            "VARCHAR(512)"
        } else {
            "TEXT"
        };
        let rows_type = if dialect == Dialect::MySql {
            "BIGINT"
        } else {
            "INTEGER"
        };
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {JOB_TABLE} (
                move_id {id_type} PRIMARY KEY,
                table_name TEXT NOT NULL,
                from_shard TEXT NOT NULL,
                to_shard TEXT NOT NULL,
                key_low TEXT,
                key_high TEXT,
                phase TEXT NOT NULL,
                cursor TEXT,
                rows_moved {rows_type} NOT NULL,
                checksum TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"
        );
        self.state.execute(&sql).await?;
        Ok(())
    }

    fn state_dialect(&self) -> Result<Dialect, ShardError> {
        match self.state.dialect() {
            Dialect::Mssql => Err(ShardError::UnsupportedDialect),
            dialect => Ok(dialect),
        }
    }

    fn client(&self, shard: &str) -> Result<Arc<dyn RdbmsClient>, ShardError> {
        self.clients
            .get(shard)
            .cloned()
            .ok_or_else(|| ShardError::Config(format!("no client for shard `{shard}`")))
    }

    async fn ensure_job(&self, mv: &ShardMove) -> Result<JobState, ShardError> {
        let id = move_id(mv);
        if let Some(job) = self.load_job(&id).await? {
            return Ok(job);
        }
        let dialect = self.state_dialect()?;
        let placeholders = (1..=11)
            .map(|n| placeholder(dialect, n))
            .collect::<Vec<_>>()
            .join(", ");
        let conflict = if dialect == Dialect::MySql {
            String::new()
        } else {
            " ON CONFLICT(move_id) DO NOTHING".to_owned()
        };
        let insert = if dialect == Dialect::MySql {
            format!("INSERT IGNORE INTO {JOB_TABLE} VALUES ({placeholders})")
        } else {
            format!("INSERT INTO {JOB_TABLE} VALUES ({placeholders}){conflict}")
        };
        let (key_low, key_high) = match mv.key_range {
            Some(range) => (
                range.low.map(|v| Value::String(v.to_string())),
                range.high.map(|v| Value::String(v.to_string())),
            ),
            None => (None, None),
        };
        let params = [
            Value::String(id.clone()),
            Value::String(mv.table.clone()),
            Value::String(mv.from.clone()),
            Value::String(mv.to.clone()),
            key_low.unwrap_or(Value::Null),
            key_high.unwrap_or(Value::Null),
            Value::String(Phase::Pending.as_str().into()),
            Value::Null,
            Value::Number(0i64.into()),
            Value::String("0".into()),
            Value::String(now_rfc3339()?),
        ];
        self.state.execute_with(&insert, &params).await?;
        self.load_job(&id)
            .await?
            .ok_or_else(|| ShardError::Database("migration job vanished after insert".to_owned()))
    }

    async fn load_job(&self, id: &str) -> Result<Option<JobState>, ShardError> {
        let dialect = self.state_dialect()?;
        let sql = format!(
            "SELECT phase, cursor, rows_moved, checksum FROM {JOB_TABLE} WHERE move_id = {}",
            placeholder(dialect, 1)
        );
        let rows = self
            .state
            .query_with(&sql, &[Value::String(id.to_owned())])
            .await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let rows_moved = u64::try_from(int(row, "rows_moved")?)
            .map_err(|_| ShardError::Database("migration rows_moved out of range".into()))?;
        let checksum = text(row, "checksum")?
            .parse::<u64>()
            .map_err(|e| ShardError::Database(format!("migration checksum is invalid: {e}")))?;
        Ok(Some(JobState {
            phase: Phase::parse(text(row, "phase")?)?,
            cursor: row.get("cursor").and_then(Value::as_str).map(str::to_owned),
            rows: rows_moved,
            checksum,
        }))
    }

    async fn save_job(&self, mv: &ShardMove, job: &JobState) -> Result<(), ShardError> {
        let dialect = self.state_dialect()?;
        let sql = format!(
            "UPDATE {JOB_TABLE} SET phase = {}, cursor = {}, rows_moved = {}, checksum = {}, updated_at = {} WHERE move_id = {}",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
            placeholder(dialect, 3),
            placeholder(dialect, 4),
            placeholder(dialect, 5),
            placeholder(dialect, 6),
        );
        let cursor = job.cursor.clone().map(Value::String).unwrap_or(Value::Null);
        let rows = i64::try_from(job.rows)
            .map_err(|_| ShardError::Database("migration rows_moved out of range".into()))?;
        let params = [
            Value::String(job.phase.as_str().into()),
            cursor,
            Value::Number(rows.into()),
            Value::String(job.checksum.to_string()),
            Value::String(now_rfc3339()?),
            Value::String(move_id(mv)),
        ];
        self.state.execute_with(&sql, &params).await?;
        Ok(())
    }

    async fn acquire_fence(&self) -> Result<Option<FenceGuard>, ShardError> {
        let Some(setting) = &self.fence else {
            return Ok(None);
        };
        setting
            .lock
            .acquire_fenced(&setting.key, setting.ttl)
            .await
            .map_err(lock_error)?
            .map(Some)
            .ok_or_else(|| ShardError::Config("another migration holds the fence".into()))
    }

    async fn check_fence(&self, guard: Option<&FenceGuard>) -> Result<(), ShardError> {
        let (Some(setting), Some(guard)) = (&self.fence, guard) else {
            return Ok(());
        };
        // 失败/竞争的 acquire 也会 INCR fence，validate_fence 会把仍持锁的
        // 调用方误判为丢失；排他性由 renew 的 token 校验保证。
        setting
            .lock
            .renew(&setting.key, &guard.token, setting.ttl)
            .await
            .map_err(lock_error)
    }

    async fn release_fence(&self, guard: Option<&FenceGuard>) -> Result<(), ShardError> {
        let (Some(setting), Some(guard)) = (&self.fence, guard) else {
            return Ok(());
        };
        setting
            .lock
            .release(&setting.key, &guard.token)
            .await
            .map_err(lock_error)
    }
}

fn lock_error(error: LockError) -> ShardError {
    ShardError::Config(format!("migration lock: {error}"))
}

fn move_id(mv: &ShardMove) -> String {
    let range = match mv.key_range {
        Some(range) => format!(
            "{}:{}",
            range
                .low
                .map(|v| v.to_string())
                .unwrap_or_else(|| "*".into()),
            range
                .high
                .map(|v| v.to_string())
                .unwrap_or_else(|| "*".into())
        ),
        None => "none".to_owned(),
    };
    format!("{}|{}|{}|{range}", mv.table, mv.from, mv.to)
}

fn now_rfc3339() -> Result<String, ShardError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| ShardError::Database(format!("failed to format migration timestamp: {e}")))
}

/// Postgres 用 `$n`，其余方言用 `?`。
fn placeholder(dialect: Dialect, n: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${n}"),
        _ => "?".into(),
    }
}

fn bind(params: &mut Vec<Value>, dialect: Dialect, value: Value) -> String {
    params.push(value);
    placeholder(dialect, params.len())
}

fn text<'a>(row: &'a Row, col: &str) -> Result<&'a str, ShardError> {
    row.get(col)
        .and_then(Value::as_str)
        .ok_or_else(|| ShardError::Database(format!("migration row missing text column `{col}`")))
}

fn int(row: &Row, col: &str) -> Result<i64, ShardError> {
    row.get(col).and_then(Value::as_i64).ok_or_else(|| {
        ShardError::Database(format!("migration row missing integer column `{col}`"))
    })
}

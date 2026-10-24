// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 归档主循环：chunk 级幂等、断点续跑，`DROP PARTITION` 前必须全部 `Verified`。
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, Row};
use serde_json::Value;
use time::macros::format_description;
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::state::{ArchiveJobState, ArchivePhase};
use crate::{ArchiveStateStore, TableTieringConfig, TieringConfig, TieringError, validate_ident};

const LOCK_KEY: &str = "ecat-data-tiering:archive";
// shortcut: 锁无续租，TTL 固定 300s；单次运行可能超过它时改成配置项。
const LOCK_TTL: StdDuration = StdDuration::from_secs(300);

/// 冷层写入/校验抽象；实现见后续 ClickHouse / S3 sink。
#[async_trait]
pub trait ColdSink: Send + Sync {
    async fn write_chunk(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        rows: &[Row],
    ) -> Result<String, TieringError>;
    async fn verify_chunk(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        checksum: &str,
    ) -> Result<bool, TieringError>;
    async fn health_check(&self) -> Result<(), TieringError>;
}

/// 单次运行的结果。`chunks`/`rows` 是本次写入冷层的量（dry-run 时为「将写入」的量）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveReport {
    pub chunks: u64,
    pub rows: u64,
    pub dropped_partitions: u64,
    pub dry_run: bool,
    pub skipped: bool,
}

pub struct ArchiveJob {
    hot: Arc<dyn RdbmsClient>,
    cold: Arc<dyn ColdSink>,
    state: ArchiveStateStore,
    config: TieringConfig,
    lock: Option<Arc<dyn ecat_lock::DistributedLock>>,
}

impl ArchiveJob {
    pub fn new(
        hot: Arc<dyn RdbmsClient>,
        cold: Arc<dyn ColdSink>,
        state: ArchiveStateStore,
        config: TieringConfig,
        lock: Option<Arc<dyn ecat_lock::DistributedLock>>,
    ) -> Self {
        Self {
            hot,
            cold,
            state,
            config,
            lock,
        }
    }

    /// 所有表各跑一轮。窗口外或锁被他人持有时返回 `skipped`，不做任何写入。
    pub async fn run_once(&self) -> Result<ArchiveReport, TieringError> {
        self.config.validate()?;
        if self.config.batch_size == 0 {
            return Err(TieringError::Config("batch_size must be > 0".into()));
        }
        let mut report = ArchiveReport {
            dry_run: self.config.dry_run,
            ..ArchiveReport::default()
        };
        if !window_allows(self.config.window_bounds()?, OffsetDateTime::now_utc()) {
            report.skipped = true;
            return Ok(report);
        }
        let token = if let Some(lock) = &self.lock {
            match lock.acquire(LOCK_KEY, LOCK_TTL).await {
                Ok(Some(token)) => Some(token),
                Ok(None) => {
                    report.skipped = true;
                    return Ok(report);
                }
                Err(e) => {
                    return Err(TieringError::State(format!(
                        "archive lock acquire failed: {e}"
                    )));
                }
            }
        } else {
            None
        };
        let result = self.archive_tables(&mut report).await;
        if let (Some(lock), Some(token)) = (&self.lock, &token) {
            // 释放失败不影响已完成的工作；锁会按 TTL 自行过期。
            let _ = lock.release(LOCK_KEY, token).await;
        }
        result?;
        Ok(report)
    }

    async fn archive_tables(&self, report: &mut ArchiveReport) -> Result<(), TieringError> {
        for table in &self.config.tables {
            validate_ident(&table.name)?;
            validate_ident(&table.time_col)?;
            validate_ident(&table.id_col)?;
            for partition in self.partitions(&table.name).await? {
                validate_ident(&partition)?;
                // 只归档整段都已过 hot_days 的分区；热冷混合的分区整体跳过。
                if self.hot_rows(table, &partition).await? > 0 {
                    continue;
                }
                self.archive_partition(table, &partition, report).await?;
            }
        }
        Ok(())
    }

    /// MySQL 物理分区名（`PARTITION_NAME` 即 `DROP PARTITION` 的入参）。
    async fn partitions(&self, table: &str) -> Result<Vec<String>, TieringError> {
        match self.hot.dialect() {
            Dialect::MySql => {}
            _ => return Err(TieringError::UnsupportedDialect),
        }
        let sql = "SELECT PARTITION_NAME AS partition_name FROM information_schema.PARTITIONS \
                   WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? \
                   AND PARTITION_NAME IS NOT NULL ORDER BY PARTITION_ORDINAL_POSITION";
        let rows = self
            .hot
            .query_with(sql, &[Value::String(table.into())])
            .await?;
        rows.iter()
            .map(|row| {
                row.get("partition_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        TieringError::State("partition query row has no `partition_name`".into())
                    })
            })
            .collect()
    }

    /// 分区内未过 `hot_days` 的行数；> 0 表示该分区还不该归档。
    async fn hot_rows(
        &self,
        table: &TableTieringConfig,
        partition: &str,
    ) -> Result<i64, TieringError> {
        let cutoff = OffsetDateTime::now_utc() - Duration::days(i64::from(self.config.hot_days));
        let sql = format!(
            "SELECT COUNT(*) AS hot_rows FROM `{}` PARTITION (`{partition}`) WHERE `{}` >= ?",
            table.name, table.time_col
        );
        let rows = self
            .hot
            .query_with(&sql, &[Value::String(fmt_time(cutoff)?)])
            .await?;
        rows.first()
            .and_then(|row| row.get("hot_rows"))
            .and_then(Value::as_i64)
            .ok_or_else(|| TieringError::State("hot_rows query returned no count".into()))
    }

    /// 走完分区内所有 chunk：写入 → `Copied` → 校验 → `Verified`；到期才 `DROP PARTITION`。
    async fn archive_partition(
        &self,
        table: &TableTieringConfig,
        partition: &str,
        report: &mut ArchiveReport,
    ) -> Result<(), TieringError> {
        let mut cursor = i64::MIN;
        let mut chunk = 0u64;
        let mut all_verified = true;
        let mut verified_at: Option<OffsetDateTime> = None;
        let mut copied_any = false;
        loop {
            let state = self.state.load(&table.name, partition, chunk).await?;
            if let Some(state) = &state {
                match state.phase {
                    // 分区已归档；不复活已 Dropped 的行。
                    ArchivePhase::Dropped => return Ok(()),
                    ArchivePhase::Verified => {
                        verified_at = Some(match verified_at {
                            Some(t) => t.max(state.updated_at),
                            None => state.updated_at,
                        });
                        copied_any = true;
                        cursor = state.high_id;
                        chunk += 1;
                        continue;
                    }
                    ArchivePhase::Copied => {
                        if self.config.dry_run {
                            cursor = state.high_id;
                            chunk += 1;
                            continue;
                        }
                        let checksum = state.checksum.as_deref().ok_or_else(|| {
                            TieringError::State(format!(
                                "{} {partition}/{chunk} is copied without checksum",
                                table.name
                            ))
                        })?;
                        if self
                            .cold
                            .verify_chunk(&table.name, partition, chunk, checksum)
                            .await?
                        {
                            self.state
                                .advance(
                                    &table.name,
                                    partition,
                                    chunk,
                                    ArchivePhase::Verified,
                                    None,
                                )
                                .await?;
                            verified_at = Some(OffsetDateTime::now_utc());
                        } else {
                            all_verified = false;
                        }
                        copied_any = true;
                        cursor = state.high_id;
                        chunk += 1;
                        continue;
                    }
                    ArchivePhase::Pending => {}
                }
            }
            let rows = self.read_chunk(table, partition, cursor).await?;
            let Some((low_id, high_id)) = id_bounds(&rows, &table.id_col)? else {
                break;
            };
            if self.config.dry_run {
                report.chunks += 1;
                report.rows += rows.len() as u64;
                cursor = high_id;
                chunk += 1;
                continue;
            }
            let checksum = self
                .cold
                .write_chunk(&table.name, partition, chunk, &rows)
                .await?;
            if state.is_none() {
                // `upsert` 只播种新行；已有行只经 `advance` 迁移（不得复活 Dropped）。
                self.state
                    .upsert(&ArchiveJobState {
                        table: table.name.clone(),
                        partition: partition.into(),
                        chunk,
                        low_id,
                        high_id,
                        rows: rows.len() as u64,
                        checksum: Some(checksum.clone()),
                        phase: ArchivePhase::Pending,
                        updated_at: OffsetDateTime::now_utc(),
                    })
                    .await?;
            }
            self.state
                .advance(
                    &table.name,
                    partition,
                    chunk,
                    ArchivePhase::Copied,
                    Some(&checksum),
                )
                .await?;
            if self
                .cold
                .verify_chunk(&table.name, partition, chunk, &checksum)
                .await?
            {
                self.state
                    .advance(&table.name, partition, chunk, ArchivePhase::Verified, None)
                    .await?;
                verified_at = Some(OffsetDateTime::now_utc());
            } else {
                all_verified = false;
            }
            report.chunks += 1;
            report.rows += rows.len() as u64;
            copied_any = true;
            cursor = high_id;
            chunk += 1;
        }
        // 带 `filter` 的分区可能仍有未归档行，DROPPED 会带走它们，所以永不 drop。
        let droppable = all_verified
            && copied_any
            && table.filter.is_none()
            && !self.config.dry_run
            && self.retention_passed(verified_at);
        if droppable {
            self.drop_partition(&table.name, partition).await?;
            for n in 0..chunk {
                self.state
                    .advance(&table.name, partition, n, ArchivePhase::Dropped, None)
                    .await?;
            }
            report.dropped_partitions += 1;
        }
        Ok(())
    }

    async fn read_chunk(
        &self,
        table: &TableTieringConfig,
        partition: &str,
        cursor: i64,
    ) -> Result<Vec<Row>, TieringError> {
        let mut sql = format!(
            "SELECT * FROM `{}` PARTITION (`{partition}`) WHERE `{}` > ?",
            table.name, table.id_col
        );
        if let Some(filter) = &table.filter {
            sql.push_str(&format!(" AND ({filter})"));
        }
        sql.push_str(&format!(" ORDER BY `{}` LIMIT ?", table.id_col));
        let params = [
            Value::Number(cursor.into()),
            Value::Number(i64::from(self.config.batch_size).into()),
        ];
        Ok(self.hot.query_with(&sql, &params).await?)
    }

    async fn drop_partition(&self, table: &str, partition: &str) -> Result<(), TieringError> {
        validate_ident(table)?;
        validate_ident(partition)?;
        self.hot
            .execute(&format!(
                "ALTER TABLE `{table}` DROP PARTITION `{partition}`"
            ))
            .await?;
        Ok(())
    }

    fn retention_passed(&self, verified_at: Option<OffsetDateTime>) -> bool {
        verified_at.is_some_and(|verified_at| {
            OffsetDateTime::now_utc()
                >= verified_at + Duration::days(i64::from(self.config.retain_s3_days))
        })
    }
}

/// `(start, end)` 为当日 UTC 分钟数；`start > end` 表示跨午夜。
fn window_allows(window: Option<(u32, u32)>, now: OffsetDateTime) -> bool {
    let Some((start, end)) = window else {
        return true;
    };
    let minute = u32::from(now.hour()) * 60 + u32::from(now.minute());
    if start < end {
        start <= minute && minute < end
    } else {
        minute >= start || minute < end
    }
}

fn id_bounds(rows: &[Row], id_col: &str) -> Result<Option<(i64, i64)>, TieringError> {
    let mut bounds: Option<(i64, i64)> = None;
    for row in rows {
        let id = row.get(id_col).and_then(Value::as_i64).ok_or_else(|| {
            TieringError::State(format!("archive chunk row has no integer `{id_col}`"))
        })?;
        bounds = Some(match bounds {
            None => (id, id),
            Some((low, high)) => (low.min(id), high.max(id)),
        });
    }
    Ok(bounds)
}

fn fmt_time(ts: OffsetDateTime) -> Result<String, TieringError> {
    ts.to_offset(UtcOffset::UTC)
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second]"
        ))
        .map_err(|e| TieringError::State(format!("failed to format cutoff: {e}")))
}

#[cfg(test)]
mod tests;

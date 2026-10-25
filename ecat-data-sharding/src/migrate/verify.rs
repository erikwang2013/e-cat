// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 回填校验：按目标分片聚合源/目标行数与 XOR checksum（与顺序无关）。
use std::collections::BTreeMap;

use super::backfill::{row_checksum, row_value};
use super::*;

impl ShardMigrator {
    /// 校验所有搬移；任一组不一致即报错且不把该组标为 `verified`。
    pub async fn verify(&self) -> Result<(), ShardError> {
        let mut groups: BTreeMap<(String, String), Vec<&ShardMove>> = BTreeMap::new();
        for mv in &self.plan.moves {
            groups
                .entry((mv.table.clone(), mv.to.clone()))
                .or_default()
                .push(mv);
        }

        for ((table_name, target_shard), moves) in groups {
            let table = self.new_catalog.table(&table_name)?;
            let mut jobs = Vec::with_capacity(moves.len());
            let mut job_rows = 0u64;
            let mut job_checksum = 0u64;
            let mut all_settled = true;
            for mv in &moves {
                let job = self.load_job(&move_id(mv)).await?.ok_or_else(|| {
                    ShardError::Config(format!(
                        "migration job for {} {}→{} is missing; run backfill first",
                        mv.table, mv.from, mv.to
                    ))
                })?;
                if !matches!(job.phase, Phase::Done | Phase::RolledBack) {
                    all_settled = false;
                }
                job_rows = job_rows.saturating_add(job.rows);
                job_checksum ^= job.checksum;
                jobs.push(job);
            }
            if all_settled {
                continue;
            }

            let mut source_rows = 0u64;
            let mut source_checksum = 0u64;
            for mv in &moves {
                let source = self.client(&mv.from)?;
                let (rows, checksum) = self.scan_move_stats(&source, table, mv).await?;
                source_rows = source_rows.saturating_add(rows);
                source_checksum ^= checksum;
            }
            let target = self.client(&target_shard)?;
            let (target_rows, target_checksum) =
                self.scan_target_stats(&target, table, &moves).await?;

            if source_rows != job_rows || source_checksum != job_checksum {
                return Err(ShardError::Config(format!(
                    "backfill incomplete for table `{table_name}` target `{target_shard}`: \
                     source {source_rows}/{:016x}, job {job_rows}/{:016x}",
                    source_checksum, job_checksum
                )));
            }
            if source_rows != target_rows || source_checksum != target_checksum {
                return Err(ShardError::Config(format!(
                    "verify mismatch for table `{table_name}` target `{target_shard}`: \
                     source {source_rows}/{:016x}, target {target_rows}/{:016x}",
                    source_checksum, target_checksum
                )));
            }

            for (mv, mut job) in moves.iter().zip(jobs) {
                job.phase = Phase::Verified;
                self.save_job(mv, &job).await?;
            }
        }
        Ok(())
    }

    /// 源侧：SQL 区间（若有）+ 新拓扑路由过滤。
    async fn scan_move_stats(
        &self,
        client: &Arc<dyn RdbmsClient>,
        table: &ShardTable,
        mv: &ShardMove,
    ) -> Result<(u64, u64), ShardError> {
        let mut cursor = None;
        let mut rows = 0u64;
        let mut checksum = 0u64;
        loop {
            let batch = self
                .scan_chunk(client, table, mv.key_range, cursor.as_ref())
                .await?;
            let Some(last) = batch.last() else {
                break;
            };
            for row in &batch {
                if !self.routes_to(mv, row)? {
                    continue;
                }
                rows = rows.saturating_add(1);
                checksum ^= row_checksum(row)?;
            }
            cursor = Some(row_value(last, &table.pk)?.clone());
        }
        Ok((rows, checksum))
    }

    /// 目标侧：不带 SQL 区间扫全分片，逐行判断是否落在任一 move 的范围内。
    async fn scan_target_stats(
        &self,
        client: &Arc<dyn RdbmsClient>,
        table: &ShardTable,
        moves: &[&ShardMove],
    ) -> Result<(u64, u64), ShardError> {
        let mut cursor = None;
        let mut rows = 0u64;
        let mut checksum = 0u64;
        loop {
            let batch = self
                .scan_chunk(client, table, None, cursor.as_ref())
                .await?;
            let Some(last) = batch.last() else {
                break;
            };
            for row in &batch {
                if !self.row_in_any_scope(table, moves, row)? {
                    continue;
                }
                rows = rows.saturating_add(1);
                checksum ^= row_checksum(row)?;
            }
            cursor = Some(row_value(last, &table.pk)?.clone());
        }
        Ok((rows, checksum))
    }

    fn row_in_any_scope(
        &self,
        table: &ShardTable,
        moves: &[&ShardMove],
        row: &Row,
    ) -> Result<bool, ShardError> {
        for mv in moves {
            let in_scope = match mv.key_range {
                None => self.routes_to(mv, row)?,
                Some(range) => {
                    let key = row_value(row, &table.key)?.as_i64().ok_or_else(|| {
                        ShardError::Database(format!(
                            "range migration key in `{}` is not an integer",
                            mv.table
                        ))
                    })?;
                    range.low.is_none_or(|low| key >= low)
                        && range.high.is_none_or(|high| key < high)
                }
            };
            if in_scope {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

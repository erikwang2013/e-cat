// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 幂等回填：按主键游标分批读源分片，按新拓扑路由后 upsert 到目标分片。
use super::*;

impl ShardMigrator {
    /// 回填全部 move；每批落库游标与累计行数/checksum，可断点续跑。
    pub async fn backfill(&self) -> Result<MigrationReport, ShardError> {
        self.ensure_schema().await?;
        let guard = self.acquire_fence().await?;
        let mut report = MigrationReport {
            moved_rows: 0,
            chunks: 0,
            verified: false,
        };
        let outcome = self.run_backfill(&mut report, guard.as_ref()).await;
        let released = self.release_fence(guard.as_ref()).await;
        outcome?;
        released?;
        Ok(report)
    }

    async fn run_backfill(
        &self,
        report: &mut MigrationReport,
        guard: Option<&FenceGuard>,
    ) -> Result<(), ShardError> {
        for mv in &self.plan.moves {
            self.backfill_move(mv, report, guard).await?;
        }
        Ok(())
    }

    async fn backfill_move(
        &self,
        mv: &ShardMove,
        report: &mut MigrationReport,
        guard: Option<&FenceGuard>,
    ) -> Result<(), ShardError> {
        let mut job = self.ensure_job(mv).await?;
        if matches!(job.phase, Phase::Verified | Phase::Cutover | Phase::Done) {
            return Ok(());
        }
        if job.phase == Phase::RolledBack {
            // 回滚后重新迁移：游标通常已在末尾；重新标记为回填中，
            // 让 verify 重新核对目标分片后再允许 cutover。
            job.phase = Phase::Backfilling;
            self.save_job(mv, &job).await?;
        }
        let source = self.client(&mv.from)?;
        let target = self.client(&mv.to)?;
        let table = self.new_catalog.table(&mv.table)?;
        let mut cursor = job.cursor.as_deref().map(parse_cursor).transpose()?;
        loop {
            self.check_fence(guard).await?;
            let batch = self
                .scan_chunk(&source, table, mv.key_range, cursor.as_ref())
                .await?;
            let Some(last) = batch.last() else {
                break;
            };
            let mut copied = 0u64;
            for row in &batch {
                if !self.routes_to(mv, row)? {
                    continue;
                }
                self.upsert_row(&target, mv, row).await?;
                job.rows = job
                    .rows
                    .checked_add(1)
                    .ok_or_else(|| ShardError::Database("migration row count overflow".into()))?;
                job.checksum ^= row_checksum(row)?;
                copied += 1;
            }
            let last_pk = row_value(last, &table.pk)?.clone();
            cursor = Some(last_pk.clone());
            job.cursor = Some(serde_json::to_string(&last_pk).map_err(|e| {
                ShardError::Database(format!("failed to encode migration cursor: {e}"))
            })?);
            job.phase = Phase::Backfilling;
            self.save_job(mv, &job).await?;
            report.moved_rows = report.moved_rows.saturating_add(copied);
            report.chunks += 1;
        }
        Ok(())
    }

    /// 主键游标分页；`range` 非空时叠加分片键区间。
    pub(super) async fn scan_chunk(
        &self,
        client: &Arc<dyn RdbmsClient>,
        table: &ShardTable,
        range: Option<KeyRange>,
        cursor: Option<&Value>,
    ) -> Result<Vec<Row>, ShardError> {
        let dialect = client.dialect();
        let mut params = Vec::new();
        let mut sql = format!("SELECT * FROM {} WHERE 1 = 1", table.name);
        if let Some(cursor) = cursor {
            let placeholder = bind(&mut params, dialect, cursor.clone());
            sql.push_str(&format!(" AND {} > {}", table.pk, placeholder));
        }
        if let Some(range) = range {
            if let Some(low) = range.low {
                let placeholder = bind(&mut params, dialect, Value::Number(low.into()));
                sql.push_str(&format!(" AND {} >= {}", table.key, placeholder));
            }
            if let Some(high) = range.high {
                let placeholder = bind(&mut params, dialect, Value::Number(high.into()));
                sql.push_str(&format!(" AND {} < {}", table.key, placeholder));
            }
        }
        sql.push_str(&format!(" ORDER BY {} LIMIT {}", table.pk, self.chunk_size));
        Ok(client.query_with(&sql, &params).await?)
    }

    /// 区间 move 已由 SQL 过滤；全表 move 按新拓扑逐行路由。
    pub(super) fn routes_to(&self, mv: &ShardMove, row: &Row) -> Result<bool, ShardError> {
        if mv.key_range.is_some() {
            return Ok(true);
        }
        let table = self.new_catalog.table(&mv.table)?;
        let key = row_value(row, &table.key)?;
        let index = self.new_catalog.shard_for(&mv.table, key)?;
        Ok(table.shards[index].name == mv.to)
    }

    /// 目标端幂等 upsert；冲突键为目录里的主键列。
    async fn upsert_row(
        &self,
        target: &Arc<dyn RdbmsClient>,
        mv: &ShardMove,
        row: &Row,
    ) -> Result<(), ShardError> {
        let dialect = target.dialect();
        if dialect == Dialect::Mssql {
            return Err(ShardError::UnsupportedDialect);
        }
        let table = self.new_catalog.table(&mv.table)?;
        let columns = row.columns();
        if columns.is_empty() {
            return Err(ShardError::Database(format!(
                "row from `{}` has no columns",
                mv.table
            )));
        }
        let placeholders = (1..=columns.len())
            .map(|n| placeholder(dialect, n))
            .collect::<Vec<_>>()
            .join(", ");
        let updates = columns
            .iter()
            .filter(|column| column.as_str() != table.pk)
            .map(|column| match dialect {
                Dialect::MySql => format!("{column} = ecat_new.{column}"),
                _ => format!("{column} = EXCLUDED.{column}"),
            })
            .collect::<Vec<_>>();
        let update = if updates.is_empty() {
            match dialect {
                Dialect::MySql => format!("{} = ecat_new.{}", table.pk, table.pk),
                _ => format!("{} = EXCLUDED.{}", table.pk, table.pk),
            }
        } else {
            updates.join(", ")
        };
        let tail = match dialect {
            Dialect::MySql => format!(" AS ecat_new ON DUPLICATE KEY UPDATE {update}"),
            _ => format!(" ON CONFLICT ({}) DO UPDATE SET {update}", table.pk),
        };
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}){tail}",
            mv.table,
            columns.join(", "),
            placeholders
        );
        target.execute_with(&sql, row.values()).await?;
        Ok(())
    }
}

fn parse_cursor(text: &str) -> Result<Value, ShardError> {
    serde_json::from_str(text)
        .map_err(|e| ShardError::Database(format!("migration cursor is invalid JSON: {e}")))
}

pub(super) fn row_value<'a>(row: &'a Row, column: &str) -> Result<&'a Value, ShardError> {
    row.get(column)
        .ok_or_else(|| ShardError::Database(format!("migration row missing column `{column}`")))
}

/// 逐行 checksum：按列名做对象规范化，列顺序无关；XOR 聚合与顺序无关。
pub(super) fn row_checksum(row: &Row) -> Result<u64, ShardError> {
    let object: serde_json::Map<String, Value> = row
        .iter()
        .map(|(column, value)| (column.to_owned(), value.clone()))
        .collect();
    crate::stable_hash64(&Value::Object(object))
}

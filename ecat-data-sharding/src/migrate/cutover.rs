// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 切流与回滚：只改路由；旧分片保留，不做反向搬移。
use super::*;
use crate::ShardRouter;

impl ShardMigrator {
    /// 全部 move 已 `verified`（或已 `done`）才 reload 新拓扑。
    pub async fn cutover(
        &self,
        router: &ShardRouter,
        new_catalog: ShardCatalog,
    ) -> Result<(), ShardError> {
        // 二进制列无法安全搬移，切流前再挡一次（防止绕过 backfill/verify 的旧状态）。
        self.preflight_schema().await?;
        for mv in &self.plan.moves {
            let job = self.load_job(&move_id(mv)).await?.ok_or_else(|| {
                ShardError::Config(format!(
                    "migration job for {} {}→{} is missing",
                    mv.table, mv.from, mv.to
                ))
            })?;
            if !matches!(job.phase, Phase::Verified | Phase::Done) {
                return Err(ShardError::Config(format!(
                    "migration job for {} {}→{} is not verified (phase `{}`)",
                    mv.table,
                    mv.from,
                    mv.to,
                    job.phase.as_str()
                )));
            }
        }
        router.reload(new_catalog)?;
        for mv in &self.plan.moves {
            if let Some(mut job) = self.load_job(&move_id(mv)).await? {
                job.phase = Phase::Done;
                self.save_job(mv, &job).await?;
            }
        }
        Ok(())
    }

    /// 切回旧拓扑并标记 `rolled_back`；数据反向搬移不在本执行器内。
    pub async fn rollback(&self, router: &ShardRouter) -> Result<(), ShardError> {
        router.reload(self.old_catalog.clone())?;
        for mv in &self.plan.moves {
            if let Some(mut job) = self.load_job(&move_id(mv)).await? {
                job.phase = Phase::RolledBack;
                self.save_job(mv, &job).await?;
            }
        }
        Ok(())
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 恢复 worker：扫描 PREPARED/COMMITTING（以及未完成的 ROLLING_BACK），
//! 按协调日志决策重放 COMMIT/ROLLBACK。
use super::coordinator::{LogRow, RecoveryReport, TwoPhaseCoordinator, XaError, XaParticipant};
use std::sync::Arc;

impl TwoPhaseCoordinator {
    /// 启动时/周期调用；带 fence 时保证同一时刻只有一个恢复者。
    pub async fn recover(&self) -> Result<RecoveryReport, XaError> {
        self.ensure_schema().await?;
        let guard = self.acquire_fence().await?;
        let outcome = self.recover_locked().await;
        let released = self.release_fence(guard.as_ref()).await;
        match outcome {
            Ok(report) => {
                released?;
                Ok(report)
            }
            Err(error) => {
                let _ = released;
                Err(error)
            }
        }
    }

    async fn recover_locked(&self) -> Result<RecoveryReport, XaError> {
        let rows = self.recoverable_logs().await?;
        let mut report = RecoveryReport {
            recovered: 0,
            failed: 0,
        };
        for row in rows {
            let participants = match self.participants_for(&row.participants) {
                Some(participants) => participants,
                None => {
                    tracing::warn!(
                        tx = %row.id,
                        "2PC recovery skipped: participant not registered"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            let commit = should_commit(&row);
            let outcome = if commit {
                self.recover_commit(&row.id, &participants).await
            } else {
                self.recover_rollback(&row.id, &participants).await
            };
            match outcome {
                Ok(()) => report.recovered += 1,
                Err(error) => {
                    tracing::warn!(tx = %row.id, %error, "2PC recovery failed; will retry");
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }

    fn participants_for(&self, names: &[String]) -> Option<Vec<Arc<dyn XaParticipant>>> {
        names
            .iter()
            .map(|name| self.participants.get(name).cloned())
            .collect()
    }

    async fn recover_commit(
        &self,
        id: &str,
        participants: &[Arc<dyn XaParticipant>],
    ) -> Result<(), XaError> {
        for participant in participants {
            participant.xa_commit(id).await?;
        }
        self.set_log(id, "committed", Some("commit")).await
    }

    async fn recover_rollback(
        &self,
        id: &str,
        participants: &[Arc<dyn XaParticipant>],
    ) -> Result<(), XaError> {
        for participant in participants.iter().rev() {
            let _ = participant.xa_end(id).await;
            participant.xa_rollback(id).await?;
        }
        self.set_log(id, "rolled_back", Some("rollback")).await
    }
}

/// `started` 未 prepare → 回滚；`prepared` 无决策 → 按提交恢复（brief 的
/// crash-after-prepare 用例）；显式 rollback 决策 → 回滚。
fn should_commit(row: &LogRow) -> bool {
    match (row.status.as_str(), row.decision.as_deref()) {
        ("started", _) => false,
        ("rolling_back", _) | ("prepared", Some("rollback")) => false,
        ("prepared", _) | ("committing", _) => true,
        _ => false,
    }
}

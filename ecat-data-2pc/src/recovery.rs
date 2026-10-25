// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 恢复 worker：扫描 STARTED/PREPARED/COMMITTING/ROLLING_BACK，
//! 按协调日志决策重放 COMMIT/ROLLBACK。
use super::coordinator::{LogRow, RecoveryReport, TwoPhaseCoordinator, XaError, XaParticipant};
use std::sync::Arc;

impl TwoPhaseCoordinator {
    /// 应在启动时、开始接受 2PC 工作之前调用；带 fence 时同一时刻只有一个恢复者。
    /// 不要在服务进程中周期调用：恢复会把本进程仍 active 的 `started` 分支按日志
    /// 回滚。崩溃进程的 active 分支随固定连接断开由数据库回滚，因此 `started` 行
    /// 只对已停止的进程有意义（或确认本进程没有任何 active 2PC 事务后再调用）。
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
            // `xa_recover` 只列 PREPARED 分支；`started` / `rolling_back` 可能包含
            // END 但未 PREPARE 的 active 分支，空列表不能证明已解决，必须直接重放。
            let cross_check = matches!(row.status.as_str(), "prepared" | "committing");
            let outcome = if commit {
                self.recover_commit(&row.id, &participants, cross_check)
                    .await
            } else {
                self.recover_rollback(&row.id, &participants, cross_check)
                    .await
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
        cross_check: bool,
    ) -> Result<(), XaError> {
        for participant in participants {
            if cross_check && already_resolved(participant.as_ref(), id).await {
                continue;
            }
            participant.xa_commit(id).await?;
        }
        self.set_log(id, "committed", Some("commit")).await
    }

    async fn recover_rollback(
        &self,
        id: &str,
        participants: &[Arc<dyn XaParticipant>],
        cross_check: bool,
    ) -> Result<(), XaError> {
        for participant in participants.iter().rev() {
            if cross_check && already_resolved(participant.as_ref(), id).await {
                continue;
            }
            let _ = participant.xa_end(id).await;
            participant.xa_rollback(id).await?;
        }
        self.set_log(id, "rolled_back", Some("rollback")).await
    }
}

/// 参与者报告没有该 XID 的 in-doubt 事务 ⇒ 已解决，跳过重放。
/// 仅当参与者 pool 固定到持有该分支的单台服务器时结论才可靠。
/// `xa_recover` 失败时返回 `false`，维持原有「按日志重放、失败下次重试」的兜底。
async fn already_resolved(participant: &dyn XaParticipant, id: &str) -> bool {
    match participant.xa_recover().await {
        Ok(in_doubt) => !in_doubt.iter().any(|xid| xid.as_str() == id),
        Err(error) => {
            tracing::debug!(tx = id, %error, "XA RECOVER failed; replaying log decision");
            false
        }
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

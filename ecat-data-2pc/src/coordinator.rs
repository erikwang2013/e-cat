// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 2PC 协调器：协调日志 + XA 参与者状态机。仅显式 API，不接入路由层。
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, Row};
use ecat_lock::{FenceGuard, FencedLock, LockError};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const TX_TABLE: &str = "distributed_tx";

#[async_trait]
pub trait XaParticipant: Send + Sync {
    async fn xa_start(&self, xid: &str) -> Result<(), XaError>;
    async fn xa_end(&self, xid: &str) -> Result<(), XaError>;
    async fn xa_prepare(&self, xid: &str) -> Result<(), XaError>;
    async fn xa_commit(&self, xid: &str) -> Result<(), XaError>;
    async fn xa_rollback(&self, xid: &str) -> Result<(), XaError>;
    async fn xa_recover(&self) -> Result<Vec<String>, XaError>;

    /// 在该参与者固定的 XA 连接上执行参数化 SQL。
    async fn execute(&self, xid: &str, sql: &str, params: &[Value]) -> Result<u64, XaError>;
}

#[derive(Debug, thiserror::Error)]
pub enum XaError {
    #[error("unsupported dialect for XA")]
    UnsupportedDialect,
    #[error("unknown XA participant `{0}`")]
    UnknownParticipant(String),
    #[error("XA transaction `{0}` is not active")]
    UnknownXid(String),
    #[error("XA state: {0}")]
    State(String),
    #[error("XA database: {0}")]
    Database(String),
    #[error("XA lock: {0}")]
    Lock(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub recovered: u32,
    pub failed: u32,
}

struct FenceSetting {
    lock: Arc<dyn FencedLock>,
    key: String,
    ttl: Duration,
}

pub struct TwoPhaseCoordinator {
    db: Arc<dyn RdbmsClient>,
    pub(crate) participants: HashMap<String, Arc<dyn XaParticipant>>,
    fence: Option<FenceSetting>,
}

/// 一个已 START 的分布式事务；`commit`/`rollback` 消费它。
pub struct DistributedTx {
    id: String,
    participants: Vec<(String, Arc<dyn XaParticipant>)>,
    db: Arc<dyn RdbmsClient>,
}

#[derive(Debug, Clone)]
pub(crate) struct LogRow {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) participants: Vec<String>,
    pub(crate) decision: Option<String>,
}

impl TwoPhaseCoordinator {
    pub fn new(
        db: Arc<dyn RdbmsClient>,
        participants: HashMap<String, Arc<dyn XaParticipant>>,
    ) -> Self {
        Self {
            db,
            participants,
            fence: None,
        }
    }

    /// 恢复 worker 单实例保护；`recover` 持有该 fence。
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

    pub async fn ensure_schema(&self) -> Result<(), XaError> {
        let dialect = self.state_dialect()?;
        let id_type = if dialect == Dialect::MySql {
            "VARCHAR(255)"
        } else {
            "TEXT"
        };
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {TX_TABLE} (
                id {id_type} PRIMARY KEY,
                status TEXT NOT NULL,
                participants TEXT NOT NULL,
                decision TEXT,
                created_at TEXT NOT NULL
            )"
        );
        self.db.execute(&sql).await.map_err(db_error)?;
        Ok(())
    }

    /// START 全部参与者并写 `started` 日志。
    pub async fn begin(
        &self,
        participants: &[(&str, Arc<dyn XaParticipant>)],
    ) -> Result<DistributedTx, XaError> {
        if participants.is_empty() {
            return Err(XaError::State(
                "2PC requires at least one participant".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (name, _) in participants {
            if !seen.insert(*name) {
                return Err(XaError::State(format!("duplicate participant `{name}`")));
            }
            if !self.participants.contains_key(*name) {
                return Err(XaError::State(format!(
                    "participant `{name}` is not registered with the coordinator"
                )));
            }
        }
        self.ensure_schema().await?;
        let id = uuid::Uuid::new_v4().to_string();
        let names: Vec<&str> = participants.iter().map(|(name, _)| *name).collect();
        self.insert_log(&id, "started", &names, None).await?;

        let mut started: Vec<(String, Arc<dyn XaParticipant>)> = Vec::new();
        for (name, participant) in participants {
            if let Err(error) = participant.xa_start(&id).await {
                // START 可能部分生效；尽力清理失败者，再回滚已启动的参与者。
                let _ = participant.xa_end(&id).await;
                let _ = participant.xa_rollback(&id).await;
                let rollback = self.rollback_started(&id, &started).await;
                if rollback.is_ok() {
                    let _ = self.set_log(&id, "rolled_back", Some("rollback")).await;
                }
                return Err(XaError::State(format!(
                    "participant `{name}` XA START failed: {error}"
                )));
            }
            started.push(((*name).to_owned(), Arc::clone(participant)));
        }
        Ok(DistributedTx {
            id,
            participants: started,
            db: Arc::clone(&self.db),
        })
    }

    async fn rollback_started(
        &self,
        id: &str,
        started: &[(String, Arc<dyn XaParticipant>)],
    ) -> Result<(), XaError> {
        for (_, participant) in started.iter().rev() {
            let _ = participant.xa_end(id).await;
            participant.xa_rollback(id).await?;
        }
        Ok(())
    }

    fn state_dialect(&self) -> Result<Dialect, XaError> {
        match self.db.dialect() {
            Dialect::Mssql => Err(XaError::UnsupportedDialect),
            dialect => Ok(dialect),
        }
    }

    async fn insert_log(
        &self,
        id: &str,
        status: &str,
        participants: &[&str],
        decision: Option<&str>,
    ) -> Result<(), XaError> {
        let dialect = self.state_dialect()?;
        let participants = serde_json::to_string(participants)
            .map_err(|e| XaError::State(format!("failed to encode participants: {e}")))?;
        let sql = format!(
            "INSERT INTO {TX_TABLE} (id, status, participants, decision, created_at) \
             VALUES ({}, {}, {}, {}, {})",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
            placeholder(dialect, 3),
            placeholder(dialect, 4),
            placeholder(dialect, 5),
        );
        let params = [
            Value::String(id.to_owned()),
            Value::String(status.to_owned()),
            Value::String(participants),
            decision.map_or(Value::Null, |d| Value::String(d.to_owned())),
            Value::String(now()?),
        ];
        self.db
            .execute_with(&sql, &params)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    pub(crate) async fn set_log(
        &self,
        id: &str,
        status: &str,
        decision: Option<&str>,
    ) -> Result<(), XaError> {
        let dialect = self.state_dialect()?;
        let sql = format!(
            "UPDATE {TX_TABLE} SET status = {}, decision = {} WHERE id = {}",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
            placeholder(dialect, 3),
        );
        let params = [
            Value::String(status.to_owned()),
            decision.map_or(Value::Null, |d| Value::String(d.to_owned())),
            Value::String(id.to_owned()),
        ];
        self.db
            .execute_with(&sql, &params)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    #[cfg(test)]
    async fn load_log(&self, id: &str) -> Result<Option<LogRow>, XaError> {
        let dialect = self.state_dialect()?;
        let sql = format!(
            "SELECT id, status, participants, decision FROM {TX_TABLE} WHERE id = {}",
            placeholder(dialect, 1)
        );
        let rows = self
            .db
            .query_with(&sql, &[Value::String(id.to_owned())])
            .await
            .map_err(db_error)?;
        rows.first().map(row_to_log).transpose()
    }

    pub(crate) async fn recoverable_logs(&self) -> Result<Vec<LogRow>, XaError> {
        let dialect = self.state_dialect()?;
        let sql = format!(
            "SELECT id, status, participants, decision FROM {TX_TABLE} \
             WHERE status IN ({}, {}, {}, {}) ORDER BY created_at",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
            placeholder(dialect, 3),
            placeholder(dialect, 4),
        );
        let params = [
            Value::String("started".into()),
            Value::String("prepared".into()),
            Value::String("committing".into()),
            Value::String("rolling_back".into()),
        ];
        let rows = self.db.query_with(&sql, &params).await.map_err(db_error)?;
        rows.iter().map(row_to_log).collect()
    }

    pub(crate) async fn acquire_fence(&self) -> Result<Option<FenceGuard>, XaError> {
        let Some(setting) = &self.fence else {
            return Ok(None);
        };
        setting
            .lock
            .acquire_fenced(&setting.key, setting.ttl)
            .await
            .map_err(lock_error)?
            .map(Some)
            .ok_or_else(|| XaError::Lock("another recovery holds the fence".into()))
    }

    pub(crate) async fn release_fence(&self, guard: Option<&FenceGuard>) -> Result<(), XaError> {
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

impl DistributedTx {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 在指定参与者的固定 XA 连接上执行一条参数化语句。
    pub async fn execute(
        &self,
        participant: &str,
        sql: &str,
        params: &[Value],
    ) -> Result<u64, XaError> {
        let participant = self
            .participants
            .iter()
            .find(|(name, _)| name == participant)
            .map(|(_, participant)| participant)
            .ok_or_else(|| XaError::UnknownParticipant(participant.to_owned()))?;
        participant.execute(&self.id, sql, params).await
    }

    /// END 全部 → PREPARE 全部 → 写 COMMIT 决策 → COMMIT 全部。
    /// 已 committed 的协调日志直接返回 Ok；COMMIT 中途失败保留 `committing` 供恢复。
    pub async fn commit(self) -> Result<(), XaError> {
        if let Some(status) = self.status().await? {
            match status.as_str() {
                "committed" => return Ok(()),
                "rolled_back" => {
                    return Err(XaError::State(format!(
                        "distributed tx `{}` was rolled back",
                        self.id
                    )));
                }
                _ => {}
            }
        }
        for (name, participant) in &self.participants {
            if let Err(error) = participant.xa_end(&self.id).await {
                let original =
                    XaError::State(format!("participant `{name}` XA END failed: {error}"));
                return self.abort(original).await;
            }
        }
        for (name, participant) in &self.participants {
            if let Err(error) = participant.xa_prepare(&self.id).await {
                let original =
                    XaError::State(format!("participant `{name}` XA PREPARE failed: {error}"));
                return self.abort(original).await;
            }
        }
        // 先落 `prepared`：即使写 COMMIT 决策前崩溃，恢复也会按 brief 的
        // crash-after-prepare 语义提交。
        self.set_log("prepared", None).await?;
        self.set_log("committing", Some("commit")).await?;
        for (name, participant) in &self.participants {
            if let Err(error) = participant.xa_commit(&self.id).await {
                return Err(XaError::State(format!(
                    "participant `{name}` XA COMMIT failed: {error}"
                )));
            }
        }
        self.set_log("committed", Some("commit")).await?;
        Ok(())
    }

    /// 全部回滚；已 rolled_back 直接 Ok。
    pub async fn rollback(self) -> Result<(), XaError> {
        if let Some(status) = self.status().await? {
            match status.as_str() {
                "rolled_back" => return Ok(()),
                "committed" => {
                    return Err(XaError::State(format!(
                        "distributed tx `{}` was committed",
                        self.id
                    )));
                }
                _ => {}
            }
        }
        self.set_log("rolling_back", Some("rollback")).await?;
        for (name, participant) in self.participants.iter().rev() {
            let _ = participant.xa_end(&self.id).await;
            if let Err(error) = participant.xa_rollback(&self.id).await {
                return Err(XaError::State(format!(
                    "participant `{name}` XA ROLLBACK failed: {error}"
                )));
            }
        }
        self.set_log("rolled_back", Some("rollback")).await?;
        Ok(())
    }

    async fn abort(self, original: XaError) -> Result<(), XaError> {
        self.set_log("rolling_back", Some("rollback")).await?;
        for (_, participant) in self.participants.iter().rev() {
            let _ = participant.xa_end(&self.id).await;
            if let Err(error) = participant.xa_rollback(&self.id).await {
                return Err(XaError::State(format!(
                    "{original}; rollback failed: {error}"
                )));
            }
        }
        self.set_log("rolled_back", Some("rollback")).await?;
        Err(original)
    }

    async fn status(&self) -> Result<Option<String>, XaError> {
        let dialect = dialect_of(&*self.db)?;
        let sql = format!(
            "SELECT status FROM {TX_TABLE} WHERE id = {}",
            placeholder(dialect, 1)
        );
        let rows = self
            .db
            .query_with(&sql, &[Value::String(self.id.clone())])
            .await
            .map_err(db_error)?;
        Ok(rows
            .first()
            .and_then(|row| row.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    async fn set_log(&self, status: &str, decision: Option<&str>) -> Result<(), XaError> {
        let dialect = dialect_of(&*self.db)?;
        let sql = format!(
            "UPDATE {TX_TABLE} SET status = {}, decision = {} WHERE id = {}",
            placeholder(dialect, 1),
            placeholder(dialect, 2),
            placeholder(dialect, 3),
        );
        let params = [
            Value::String(status.to_owned()),
            decision.map_or(Value::Null, |d| Value::String(d.to_owned())),
            Value::String(self.id.clone()),
        ];
        self.db
            .execute_with(&sql, &params)
            .await
            .map_err(db_error)?;
        Ok(())
    }
}

fn row_to_log(row: &Row) -> Result<LogRow, XaError> {
    let participants = row
        .get("participants")
        .and_then(Value::as_str)
        .ok_or_else(|| XaError::State("distributed_tx.participants missing".into()))?;
    Ok(LogRow {
        id: row
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| XaError::State("distributed_tx.id missing".into()))?
            .to_owned(),
        status: row
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| XaError::State("distributed_tx.status missing".into()))?
            .to_owned(),
        participants: serde_json::from_str(participants)
            .map_err(|e| XaError::State(format!("invalid participants JSON: {e}")))?,
        decision: row
            .get("decision")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn dialect_of(db: &dyn RdbmsClient) -> Result<Dialect, XaError> {
    match db.dialect() {
        Dialect::Mssql => Err(XaError::UnsupportedDialect),
        dialect => Ok(dialect),
    }
}

fn placeholder(dialect: Dialect, n: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${n}"),
        _ => "?".into(),
    }
}

fn db_error(error: ecat_data::RdbmsError) -> XaError {
    XaError::Database(error.to_string())
}

fn lock_error(error: LockError) -> XaError {
    XaError::Lock(error.to_string())
}

fn now() -> Result<String, XaError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| XaError::State(format!("failed to format timestamp: {e}")))
}

#[cfg(test)]
mod tests;

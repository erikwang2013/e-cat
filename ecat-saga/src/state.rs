// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Saga 状态存储：内存实现与 SQL（SQLite/MySQL/PostgreSQL）实现。
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, Row};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::Mutex;

use crate::saga::SagaError;

const INSTANCES: &str = "saga_instances";
const STEPS: &str = "saga_steps";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SagaStatus {
    Running,
    Compensating,
    Completed,
    Compensated,
    Failed,
}

impl SagaStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Compensating => "compensating",
            Self::Completed => "completed",
            Self::Compensated => "compensated",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Result<Self, SagaError> {
        match text {
            "running" => Ok(Self::Running),
            "compensating" => Ok(Self::Compensating),
            "completed" => Ok(Self::Completed),
            "compensated" => Ok(Self::Compensated),
            "failed" => Ok(Self::Failed),
            other => Err(SagaError::State(format!("unknown saga status `{other}`"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Done,
    Compensated,
    Failed,
}

impl StepStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Compensated => "compensated",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Result<Self, SagaError> {
        match text {
            "pending" => Ok(Self::Pending),
            "done" => Ok(Self::Done),
            "compensated" => Ok(Self::Compensated),
            "failed" => Ok(Self::Failed),
            other => Err(SagaError::State(format!("unknown step status `{other}`"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SagaState {
    pub saga_id: String,
    pub status: SagaStatus,
    pub current_step: usize,
    pub context: Value,
}

#[async_trait]
pub trait SagaStateStore: Send + Sync {
    async fn load(&self, saga_id: &str) -> Result<Option<SagaState>, SagaError>;
    async fn save(&self, state: &SagaState) -> Result<(), SagaError>;
    async fn step_status(&self, saga_id: &str, step: &str) -> Result<StepStatus, SagaError>;
    async fn set_step(
        &self,
        saga_id: &str,
        step: &str,
        status: StepStatus,
    ) -> Result<(), SagaError>;
}

/// 进程内状态：测试与单机场景；重启后不保留。
#[derive(Default)]
pub struct MemorySagaStateStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    states: HashMap<String, SagaState>,
    steps: HashMap<(String, String), StepStatus>,
}

impl MemorySagaStateStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SagaStateStore for MemorySagaStateStore {
    async fn load(&self, saga_id: &str) -> Result<Option<SagaState>, SagaError> {
        Ok(self.inner.lock().await.states.get(saga_id).cloned())
    }

    async fn save(&self, state: &SagaState) -> Result<(), SagaError> {
        self.inner
            .lock()
            .await
            .states
            .insert(state.saga_id.clone(), state.clone());
        Ok(())
    }

    async fn step_status(&self, saga_id: &str, step: &str) -> Result<StepStatus, SagaError> {
        Ok(self
            .inner
            .lock()
            .await
            .steps
            .get(&(saga_id.to_owned(), step.to_owned()))
            .copied()
            .unwrap_or(StepStatus::Pending))
    }

    async fn set_step(
        &self,
        saga_id: &str,
        step: &str,
        status: StepStatus,
    ) -> Result<(), SagaError> {
        self.inner
            .lock()
            .await
            .steps
            .insert((saga_id.to_owned(), step.to_owned()), status);
        Ok(())
    }
}

/// SQL 状态存储；`Saga::run` 前调用 [`SqlSagaStateStore::ensure_schema`]。
pub struct SqlSagaStateStore {
    db: Arc<dyn RdbmsClient>,
}

impl SqlSagaStateStore {
    pub fn new(db: Arc<dyn RdbmsClient>) -> Self {
        Self { db }
    }

    pub async fn ensure_schema(&self) -> Result<(), SagaError> {
        let dialect = self.dialect()?;
        let id_type = if dialect == Dialect::MySql {
            "VARCHAR(255)"
        } else {
            "TEXT"
        };
        self.db
            .execute(&format!(
                "CREATE TABLE IF NOT EXISTS {INSTANCES} (
                    saga_id {id_type} PRIMARY KEY,
                    status TEXT NOT NULL,
                    current_step BIGINT NOT NULL,
                    context TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )"
            ))
            .await
            .map_err(db_error)?;
        self.db
            .execute(&format!(
                "CREATE TABLE IF NOT EXISTS {STEPS} (
                    saga_id {id_type} NOT NULL,
                    step_name {id_type} NOT NULL,
                    status TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (saga_id, step_name)
                )"
            ))
            .await
            .map_err(db_error)?;
        Ok(())
    }

    fn dialect(&self) -> Result<Dialect, SagaError> {
        match self.db.dialect() {
            Dialect::Mssql => Err(SagaError::State("MSSQL saga store is unsupported".into())),
            dialect => Ok(dialect),
        }
    }
}

#[async_trait]
impl SagaStateStore for SqlSagaStateStore {
    async fn load(&self, saga_id: &str) -> Result<Option<SagaState>, SagaError> {
        let dialect = self.dialect()?;
        let sql = format!(
            "SELECT saga_id, status, current_step, context FROM {INSTANCES} WHERE saga_id = {}",
            placeholder(dialect, 1)
        );
        let rows = self
            .db
            .query_with(&sql, &[Value::String(saga_id.to_owned())])
            .await
            .map_err(db_error)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let current_step = u64::try_from(int(row, "current_step")?)
            .map_err(|_| SagaError::State("saga current_step out of range".into()))?;
        let context = text(row, "context")?;
        Ok(Some(SagaState {
            saga_id: text(row, "saga_id")?.to_owned(),
            status: SagaStatus::parse(text(row, "status")?)?,
            current_step: usize::try_from(current_step)
                .map_err(|_| SagaError::State("saga current_step out of range".into()))?,
            context: serde_json::from_str(context)
                .map_err(|e| SagaError::State(format!("saga context is invalid JSON: {e}")))?,
        }))
    }

    async fn save(&self, state: &SagaState) -> Result<(), SagaError> {
        let dialect = self.dialect()?;
        let current_step = i64::try_from(state.current_step)
            .map_err(|_| SagaError::State("saga current_step out of range".into()))?;
        let context = serde_json::to_string(&state.context)
            .map_err(|e| SagaError::State(format!("failed to encode saga context: {e}")))?;
        let sql = match dialect {
            Dialect::MySql => format!(
                "INSERT INTO {INSTANCES} (saga_id, status, current_step, context, updated_at) \
                 VALUES (?, ?, ?, ?, ?) AS ecat_new \
                 ON DUPLICATE KEY UPDATE status = ecat_new.status, \
                 current_step = ecat_new.current_step, context = ecat_new.context, \
                 updated_at = ecat_new.updated_at"
            ),
            _ => format!(
                "INSERT INTO {INSTANCES} (saga_id, status, current_step, context, updated_at) \
                 VALUES ({}, {}, {}, {}, {}) ON CONFLICT(saga_id) DO UPDATE SET \
                 status = EXCLUDED.status, current_step = EXCLUDED.current_step, \
                 context = EXCLUDED.context, updated_at = EXCLUDED.updated_at",
                placeholder(dialect, 1),
                placeholder(dialect, 2),
                placeholder(dialect, 3),
                placeholder(dialect, 4),
                placeholder(dialect, 5),
            ),
        };
        let params = [
            Value::String(state.saga_id.clone()),
            Value::String(state.status.as_str().into()),
            Value::Number(current_step.into()),
            Value::String(context),
            Value::String(now()?),
        ];
        self.db
            .execute_with(&sql, &params)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn step_status(&self, saga_id: &str, step: &str) -> Result<StepStatus, SagaError> {
        let dialect = self.dialect()?;
        let sql = format!(
            "SELECT status FROM {STEPS} WHERE saga_id = {} AND step_name = {}",
            placeholder(dialect, 1),
            placeholder(dialect, 2)
        );
        let rows = self
            .db
            .query_with(
                &sql,
                &[
                    Value::String(saga_id.to_owned()),
                    Value::String(step.to_owned()),
                ],
            )
            .await
            .map_err(db_error)?;
        match rows.first() {
            Some(row) => StepStatus::parse(text(row, "status")?),
            None => Ok(StepStatus::Pending),
        }
    }

    async fn set_step(
        &self,
        saga_id: &str,
        step: &str,
        status: StepStatus,
    ) -> Result<(), SagaError> {
        let dialect = self.dialect()?;
        let sql = match dialect {
            Dialect::MySql => format!(
                "INSERT INTO {STEPS} (saga_id, step_name, status, updated_at) \
                 VALUES (?, ?, ?, ?) AS ecat_new \
                 ON DUPLICATE KEY UPDATE status = ecat_new.status, \
                 updated_at = ecat_new.updated_at"
            ),
            _ => format!(
                "INSERT INTO {STEPS} (saga_id, step_name, status, updated_at) \
                 VALUES ({}, {}, {}, {}) ON CONFLICT(saga_id, step_name) DO UPDATE SET \
                 status = EXCLUDED.status, updated_at = EXCLUDED.updated_at",
                placeholder(dialect, 1),
                placeholder(dialect, 2),
                placeholder(dialect, 3),
                placeholder(dialect, 4),
            ),
        };
        let params = [
            Value::String(saga_id.to_owned()),
            Value::String(step.to_owned()),
            Value::String(status.as_str().into()),
            Value::String(now()?),
        ];
        self.db
            .execute_with(&sql, &params)
            .await
            .map_err(db_error)?;
        Ok(())
    }
}

fn placeholder(dialect: Dialect, n: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${n}"),
        _ => "?".into(),
    }
}

fn db_error(error: ecat_data::RdbmsError) -> SagaError {
    SagaError::State(error.to_string())
}

fn now() -> Result<String, SagaError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| SagaError::State(format!("failed to format timestamp: {e}")))
}

fn text<'a>(row: &'a Row, column: &str) -> Result<&'a str, SagaError> {
    row.get(column)
        .and_then(Value::as_str)
        .ok_or_else(|| SagaError::State(format!("saga row missing text column `{column}`")))
}

fn int(row: &Row, column: &str) -> Result<i64, SagaError> {
    row.get(column)
        .and_then(Value::as_i64)
        .ok_or_else(|| SagaError::State(format!("saga row missing integer column `{column}`")))
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Saga 行为测试：顺序执行、逆序补偿、持久化续跑与失败上报。
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::RdbmsClient;
use ecat_data_sqlx::{PoolParams, SqlxClient};
use serde_json::json;
use tokio::sync::Mutex;

use crate::{
    MemorySagaStateStore, Saga, SagaContext, SagaError, SagaState, SagaStateStore, SagaStatus,
    SagaStep, SqlSagaStateStore, StepStatus,
};

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    async fn push(&self, entry: String) {
        self.0.lock().await.push(entry);
    }

    async fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().await)
    }
}

struct TestStep {
    name: &'static str,
    log: Log,
    fail_execute: bool,
    fail_compensate: bool,
}

#[async_trait]
impl SagaStep for TestStep {
    fn name(&self) -> &str {
        self.name
    }

    async fn execute(&self, _ctx: &mut SagaContext) -> Result<(), SagaError> {
        self.log.push(format!("exec:{}", self.name)).await;
        if self.fail_execute {
            return Err(SagaError::State(format!("{} boom", self.name)));
        }
        Ok(())
    }

    async fn compensate(&self, _ctx: &mut SagaContext) -> Result<(), SagaError> {
        self.log.push(format!("comp:{}", self.name)).await;
        if self.fail_compensate {
            return Err(SagaError::State(format!("{} compensate boom", self.name)));
        }
        Ok(())
    }
}

fn steps(log: &Log, defs: &[(&'static str, bool, bool)]) -> Vec<Arc<dyn SagaStep>> {
    defs.iter()
        .map(|(name, fail_execute, fail_compensate)| {
            Arc::new(TestStep {
                name,
                log: log.clone(),
                fail_execute: *fail_execute,
                fail_compensate: *fail_compensate,
            }) as Arc<dyn SagaStep>
        })
        .collect()
}

fn context(saga_id: &str) -> SagaContext {
    SagaContext {
        saga_id: saga_id.to_owned(),
        idempotency_key: format!("key-{saga_id}"),
        data: json!({}),
    }
}

#[tokio::test]
async fn saga_success_runs_steps_in_order() {
    let log = Log::default();
    let store = Arc::new(MemorySagaStateStore::new());
    let saga = Saga::new(
        steps(
            &log,
            &[
                ("a", false, false),
                ("b", false, false),
                ("c", false, false),
            ],
        ),
        store.clone(),
    );
    saga.run("s1", context("s1")).await.unwrap();
    assert_eq!(log.take().await, vec!["exec:a", "exec:b", "exec:c"]);
    let state = store.load("s1").await.unwrap().unwrap();
    assert_eq!(state.status, SagaStatus::Completed);
    assert_eq!(state.current_step, 3);
    assert_eq!(
        store.step_status("s1", "a").await.unwrap(),
        StepStatus::Done
    );
}

#[tokio::test]
async fn saga_failure_compensates_in_reverse() {
    let log = Log::default();
    let store = Arc::new(MemorySagaStateStore::new());
    let saga = Saga::new(
        steps(
            &log,
            &[("a", false, false), ("b", false, false), ("c", true, false)],
        ),
        store.clone(),
    );
    let err = saga.run("s1", context("s1")).await.unwrap_err();
    assert!(err.to_string().contains("c"), "got: {err}");
    assert_eq!(
        log.take().await,
        vec!["exec:a", "exec:b", "exec:c", "comp:b", "comp:a"]
    );
    let state = store.load("s1").await.unwrap().unwrap();
    assert_eq!(state.status, SagaStatus::Compensated);
    assert_eq!(
        store.step_status("s1", "a").await.unwrap(),
        StepStatus::Compensated
    );
    assert_eq!(
        store.step_status("s1", "b").await.unwrap(),
        StepStatus::Compensated
    );
    assert_eq!(
        store.step_status("s1", "c").await.unwrap(),
        StepStatus::Failed
    );
}

#[tokio::test]
async fn saga_resume_continues_from_persisted_step() {
    let log = Log::default();
    let store = Arc::new(MemorySagaStateStore::new());
    let ctx = context("s1");
    store
        .save(&SagaState {
            saga_id: "s1".into(),
            status: SagaStatus::Running,
            current_step: 1,
            context: serde_json::to_value(&ctx).unwrap(),
        })
        .await
        .unwrap();
    store.set_step("s1", "a", StepStatus::Done).await.unwrap();

    let saga = Saga::new(
        steps(
            &log,
            &[
                ("a", false, false),
                ("b", false, false),
                ("c", false, false),
            ],
        ),
        store.clone(),
    );
    saga.resume("s1").await.unwrap();
    assert_eq!(log.take().await, vec!["exec:b", "exec:c"]);
    assert_eq!(
        store.load("s1").await.unwrap().unwrap().status,
        SagaStatus::Completed
    );
}

#[tokio::test]
async fn compensation_failure_is_reported() {
    let log = Log::default();
    let store = Arc::new(MemorySagaStateStore::new());
    let saga = Saga::new(
        steps(
            &log,
            &[("a", false, true), ("b", false, false), ("c", true, false)],
        ),
        store.clone(),
    );
    let err = saga.run("s1", context("s1")).await.unwrap_err();
    match err {
        SagaError::CompensationFailed { step, .. } => assert_eq!(step, "a"),
        other => panic!("expected CompensationFailed, got {other}"),
    }
    assert_eq!(
        log.take().await,
        vec!["exec:a", "exec:b", "exec:c", "comp:b", "comp:a"]
    );
    assert_eq!(
        store.load("s1").await.unwrap().unwrap().status,
        SagaStatus::Failed
    );
}

#[tokio::test]
async fn duplicate_run_is_idempotent() {
    let log = Log::default();
    let store = Arc::new(MemorySagaStateStore::new());
    let saga = Saga::new(
        steps(&log, &[("a", false, false), ("b", false, false)]),
        store.clone(),
    );
    saga.run("s1", context("s1")).await.unwrap();
    saga.run("s1", context("s1")).await.unwrap();
    assert_eq!(log.take().await, vec!["exec:a", "exec:b"]);
    assert_eq!(
        store.load("s1").await.unwrap().unwrap().status,
        SagaStatus::Completed
    );
}

#[tokio::test]
async fn saga_sql_store_roundtrip() {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    let db: Arc<dyn RdbmsClient> = Arc::new(
        SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap(),
    );
    let store = Arc::new(SqlSagaStateStore::new(db));
    store.ensure_schema().await.unwrap();

    let log = Log::default();
    let saga = Saga::new(
        steps(&log, &[("a", false, false), ("b", false, false)]),
        store.clone(),
    );
    saga.run("s1", context("s1")).await.unwrap();
    assert_eq!(log.take().await, vec!["exec:a", "exec:b"]);

    let state = store.load("s1").await.unwrap().unwrap();
    assert_eq!(state.status, SagaStatus::Completed);
    assert_eq!(state.current_step, 2);
    assert_eq!(
        store.step_status("s1", "a").await.unwrap(),
        StepStatus::Done
    );
    assert_eq!(
        store.step_status("s1", "b").await.unwrap(),
        StepStatus::Done
    );
}

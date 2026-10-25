// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 2PC 状态机测试：fake 参与者 + SQLite 协调日志（不连真 MySQL/PG）。
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::RdbmsClient;
use ecat_data_sqlx::{PoolParams, SqlxClient};
use serde_json::Value;
use tokio::sync::Mutex;

use super::{RecoveryReport, TwoPhaseCoordinator, XaError, XaParticipant};
use crate::SqlxXaParticipant;

#[derive(Default)]
struct FakeState {
    log: Vec<String>,
    fail_start: bool,
    fail_prepare: bool,
}

struct FakeXa {
    state: Arc<Mutex<FakeState>>,
}

#[async_trait]
impl XaParticipant for FakeXa {
    async fn xa_start(&self, _xid: &str) -> Result<(), XaError> {
        let mut state = self.state.lock().await;
        state.log.push("start".into());
        if state.fail_start {
            return Err(XaError::State("start failed".into()));
        }
        Ok(())
    }

    async fn xa_end(&self, _xid: &str) -> Result<(), XaError> {
        self.state.lock().await.log.push("end".into());
        Ok(())
    }

    async fn xa_prepare(&self, _xid: &str) -> Result<(), XaError> {
        let mut state = self.state.lock().await;
        state.log.push("prepare".into());
        if state.fail_prepare {
            return Err(XaError::State("prepare failed".into()));
        }
        Ok(())
    }

    async fn xa_commit(&self, _xid: &str) -> Result<(), XaError> {
        self.state.lock().await.log.push("commit".into());
        Ok(())
    }

    async fn xa_rollback(&self, _xid: &str) -> Result<(), XaError> {
        self.state.lock().await.log.push("rollback".into());
        Ok(())
    }

    async fn xa_recover(&self) -> Result<Vec<String>, XaError> {
        Ok(vec!["in-doubt-xid".into()])
    }

    async fn execute(&self, _xid: &str, sql: &str, _params: &[Value]) -> Result<u64, XaError> {
        self.state.lock().await.log.push(format!("exec:{sql}"));
        Ok(1)
    }
}

fn fake(fail_prepare: bool) -> (Arc<dyn XaParticipant>, Arc<Mutex<FakeState>>) {
    let state = Arc::new(Mutex::new(FakeState {
        fail_prepare,
        ..FakeState::default()
    }));
    let participant = Arc::new(FakeXa {
        state: Arc::clone(&state),
    });
    (participant, state)
}

async fn db() -> Arc<dyn RdbmsClient> {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    Arc::new(
        SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap(),
    )
}

async fn log(state: &Arc<Mutex<FakeState>>) -> Vec<String> {
    state.lock().await.log.clone()
}

fn registry(
    p1: Arc<dyn XaParticipant>,
    p2: Arc<dyn XaParticipant>,
) -> HashMap<String, Arc<dyn XaParticipant>> {
    let mut registry = HashMap::new();
    registry.insert("p1".into(), p1);
    registry.insert("p2".into(), p2);
    registry
}

#[tokio::test]
async fn all_prepare_success_commits_all() {
    let (p1, log1) = fake(false);
    let (p2, log2) = fake(false);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p1.clone(), p2.clone()));
    let tx = coordinator.begin(&[("p1", p1), ("p2", p2)]).await.unwrap();
    let id = tx.id().to_owned();
    tx.execute("p1", "INSERT INTO t VALUES (?)", &[serde_json::json!(1)])
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        log(&log1).await,
        [
            "start",
            "exec:INSERT INTO t VALUES (?)",
            "end",
            "prepare",
            "commit"
        ]
    );
    assert_eq!(log(&log2).await, ["start", "end", "prepare", "commit"]);
    let row = coordinator.load_log(&id).await.unwrap().unwrap();
    assert_eq!(row.status, "committed");
    assert_eq!(row.decision.as_deref(), Some("commit"));
}

#[tokio::test]
async fn prepare_failure_rolls_back_all() {
    let (p1, log1) = fake(false);
    let (p2, log2) = fake(true);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p1.clone(), p2.clone()));
    let tx = coordinator.begin(&[("p1", p1), ("p2", p2)]).await.unwrap();
    let id = tx.id().to_owned();
    assert!(tx.commit().await.is_err());

    for log in [log(&log1).await, log(&log2).await] {
        assert_eq!(
            log.iter().filter(|entry| *entry == "rollback").count(),
            1,
            "participant must be rolled back: {log:?}"
        );
        assert_eq!(log.last().map(String::as_str), Some("rollback"), "{log:?}");
    }
    let row = coordinator.load_log(&id).await.unwrap().unwrap();
    assert_eq!(row.status, "rolled_back");
    assert_eq!(row.decision.as_deref(), Some("rollback"));
}

#[tokio::test]
async fn coordinator_crash_after_prepare_recovers_commit() {
    let (p1, log1) = fake(false);
    let (p2, log2) = fake(false);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p1, p2));
    coordinator.ensure_schema().await.unwrap();
    coordinator
        .insert_log("tx-1", "prepared", &["p1", "p2"], Some("commit"))
        .await
        .unwrap();

    let report = coordinator.recover().await.unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            recovered: 1,
            failed: 0
        }
    );
    assert!(log(&log1).await.contains(&"commit".to_owned()));
    assert!(log(&log2).await.contains(&"commit".to_owned()));
    let row = coordinator.load_log("tx-1").await.unwrap().unwrap();
    assert_eq!(row.status, "committed");
}

#[tokio::test]
async fn commit_and_rollback_are_idempotent() {
    let (p1, log1) = fake(false);
    let (p2, log2) = fake(false);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p1, p2));
    coordinator.ensure_schema().await.unwrap();
    coordinator
        .insert_log("tx-commit", "prepared", &["p1", "p2"], Some("commit"))
        .await
        .unwrap();
    assert_eq!(coordinator.recover().await.unwrap().recovered, 1);
    assert_eq!(coordinator.recover().await.unwrap().recovered, 0);
    assert_eq!(
        log(&log1)
            .await
            .iter()
            .filter(|entry| *entry == "commit")
            .count(),
        1
    );
    assert_eq!(
        log(&log2)
            .await
            .iter()
            .filter(|entry| *entry == "commit")
            .count(),
        1
    );

    let (p3, log3) = fake(false);
    let (p4, log4) = fake(false);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p3, p4));
    coordinator.ensure_schema().await.unwrap();
    coordinator
        .insert_log(
            "tx-rollback",
            "rolling_back",
            &["p1", "p2"],
            Some("rollback"),
        )
        .await
        .unwrap();
    assert_eq!(coordinator.recover().await.unwrap().recovered, 1);
    assert_eq!(coordinator.recover().await.unwrap().recovered, 0);
    for log in [log(&log3).await, log(&log4).await] {
        assert_eq!(
            log.iter().filter(|entry| *entry == "rollback").count(),
            1,
            "{log:?}"
        );
    }
}

#[tokio::test]
async fn xa_recover_lists_in_doubt() {
    let (participant, _) = fake(false);
    let in_doubt = participant.xa_recover().await.unwrap();
    assert_eq!(in_doubt, vec!["in-doubt-xid"]);
}

#[tokio::test]
async fn unsupported_dialect_is_rejected() {
    let client = SqlxClient::connect("sqlite::memory:").await.unwrap();
    match SqlxXaParticipant::new(&client) {
        Err(XaError::UnsupportedDialect) => {}
        Err(other) => panic!("expected UnsupportedDialect, got {other}"),
        Ok(_) => panic!("SQLite must not be accepted as an XA participant"),
    }
}

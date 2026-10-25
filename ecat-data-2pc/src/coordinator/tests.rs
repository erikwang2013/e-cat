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
#[cfg(feature = "sqlx")]
use crate::SqlxXaParticipant;

#[derive(Default)]
struct FakeState {
    log: Vec<String>,
    fail_start: bool,
    fail_prepare: bool,
    fail_commit: bool,
    fail_recover: bool,
    in_doubt: Vec<String>,
}

struct FakeXa {
    state: Arc<Mutex<FakeState>>,
}

#[async_trait]
impl XaParticipant for FakeXa {
    async fn xa_start(&self, xid: &str) -> Result<(), XaError> {
        let mut state = self.state.lock().await;
        state.log.push("start".into());
        if state.fail_start {
            return Err(XaError::State("start failed".into()));
        }
        state.in_doubt.push(xid.to_owned());
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

    async fn xa_commit(&self, xid: &str) -> Result<(), XaError> {
        let mut state = self.state.lock().await;
        state.log.push("commit".into());
        state.in_doubt.retain(|known| known != xid);
        if state.fail_commit {
            return Err(XaError::State("commit failed".into()));
        }
        Ok(())
    }

    async fn xa_rollback(&self, xid: &str) -> Result<(), XaError> {
        let mut state = self.state.lock().await;
        state.log.push("rollback".into());
        state.in_doubt.retain(|known| known != xid);
        Ok(())
    }

    async fn xa_recover(&self) -> Result<Vec<String>, XaError> {
        let state = self.state.lock().await;
        if state.fail_recover {
            return Err(XaError::State("recover failed".into()));
        }
        Ok(state.in_doubt.clone())
    }

    async fn execute(&self, _xid: &str, sql: &str, _params: &[Value]) -> Result<u64, XaError> {
        self.state.lock().await.log.push(format!("exec:{sql}"));
        Ok(1)
    }
}

fn fake_with(state: FakeState) -> (Arc<dyn XaParticipant>, Arc<Mutex<FakeState>>) {
    let state = Arc::new(Mutex::new(state));
    let participant = Arc::new(FakeXa {
        state: Arc::clone(&state),
    });
    (participant, state)
}

fn fake(fail_prepare: bool) -> (Arc<dyn XaParticipant>, Arc<Mutex<FakeState>>) {
    fake_with(FakeState {
        fail_prepare,
        ..FakeState::default()
    })
}

fn fake_with_in_doubt(in_doubt: &[&str]) -> (Arc<dyn XaParticipant>, Arc<Mutex<FakeState>>) {
    fake_with(FakeState {
        in_doubt: in_doubt.iter().map(|xid| (*xid).to_owned()).collect(),
        ..FakeState::default()
    })
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
    let (p1, log1) = fake_with_in_doubt(&["tx-1"]);
    let (p2, log2) = fake_with_in_doubt(&["tx-1"]);
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
    let (p1, log1) = fake_with_in_doubt(&["tx-commit"]);
    let (p2, log2) = fake_with_in_doubt(&["tx-commit"]);
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

    let (p3, log3) = fake_with_in_doubt(&["tx-rollback"]);
    let (p4, log4) = fake_with_in_doubt(&["tx-rollback"]);
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
    let (participant, _) = fake_with_in_doubt(&["in-doubt-xid"]);
    let in_doubt = participant.xa_recover().await.unwrap();
    assert_eq!(in_doubt, vec!["in-doubt-xid"]);
}

fn registry_of(
    entries: &[(&str, Arc<dyn XaParticipant>)],
) -> HashMap<String, Arc<dyn XaParticipant>> {
    entries
        .iter()
        .map(|(name, participant)| ((*name).to_owned(), Arc::clone(participant)))
        .collect()
}

async fn seed_log(
    coordinator: &TwoPhaseCoordinator,
    id: &str,
    status: &str,
    decision: Option<&str>,
) {
    coordinator.ensure_schema().await.unwrap();
    coordinator
        .insert_log(id, status, &["p1", "p2"], decision)
        .await
        .unwrap();
}

#[tokio::test]
async fn recovery_commits_participants_listing_the_xid() {
    let (p1, log1) = fake_with_in_doubt(&["tx-present"]);
    let (p2, log2) = fake_with_in_doubt(&["tx-present"]);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p1, p2));
    seed_log(&coordinator, "tx-present", "committing", Some("commit")).await;

    let report = coordinator.recover().await.unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            recovered: 1,
            failed: 0
        }
    );
    for log in [log(&log1).await, log(&log2).await] {
        assert_eq!(
            log.iter().filter(|entry| *entry == "commit").count(),
            1,
            "in-doubt participant must be committed once: {log:?}"
        );
    }
}

#[tokio::test]
async fn recovery_skips_participants_without_the_in_doubt_xid() {
    let (p1, log1) = fake_with_in_doubt(&["tx-absent"]);
    let (p2, log2) = fake_with_in_doubt(&[]);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p1, p2));
    seed_log(&coordinator, "tx-absent", "prepared", Some("commit")).await;

    let report = coordinator.recover().await.unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            recovered: 1,
            failed: 0
        }
    );
    assert_eq!(
        log(&log1)
            .await
            .iter()
            .filter(|entry| *entry == "commit")
            .count(),
        1
    );
    assert!(
        !log(&log2).await.contains(&"commit".to_owned()),
        "participant without the XID must not be reissued: {:?}",
        log(&log2).await
    );
    let row = coordinator.load_log("tx-absent").await.unwrap().unwrap();
    assert_eq!(row.status, "committed");
}

#[tokio::test]
async fn recovery_skips_rollback_participants_without_the_in_doubt_xid() {
    let (p1, log1) = fake_with_in_doubt(&["tx-absent-rollback"]);
    let (p2, log2) = fake_with_in_doubt(&[]);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry(p1, p2));
    seed_log(
        &coordinator,
        "tx-absent-rollback",
        "rolling_back",
        Some("rollback"),
    )
    .await;

    let report = coordinator.recover().await.unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            recovered: 1,
            failed: 0
        }
    );
    assert_eq!(
        log(&log1)
            .await
            .iter()
            .filter(|entry| *entry == "rollback")
            .count(),
        1
    );
    assert!(
        !log(&log2).await.contains(&"rollback".to_owned()),
        "participant without the XID must not be reissued: {:?}",
        log(&log2).await
    );
    let row = coordinator
        .load_log("tx-absent-rollback")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "rolled_back");
}

#[tokio::test]
async fn recovery_replays_log_when_xa_recover_fails() {
    let (p1, state1) = fake_with(FakeState {
        fail_recover: true,
        fail_commit: true,
        ..FakeState::default()
    });
    let (p2, _) = fake_with_in_doubt(&["tx-recover-error"]);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry_of(&[("p1", p1), ("p2", p2)]));
    seed_log(&coordinator, "tx-recover-error", "prepared", Some("commit")).await;

    let report = coordinator.recover().await.unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            recovered: 0,
            failed: 1
        },
        "xa_recover failure must fall back to replay and report the failed attempt"
    );
    assert!(
        log(&state1).await.contains(&"commit".to_owned()),
        "recover error must not suppress the log replay"
    );
    let row = coordinator
        .load_log("tx-recover-error")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status, "prepared",
        "failed recovery must stay retryable"
    );

    state1.lock().await.fail_commit = false;
    let report = coordinator.recover().await.unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            recovered: 1,
            failed: 0
        }
    );
    let row = coordinator
        .load_log("tx-recover-error")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "committed");
}

#[tokio::test]
async fn started_recovery_rolls_back_without_in_doubt_cross_check() {
    // `started` 的 XID 是 active 事务，XA RECOVER 不会列出；仍须按日志回滚。
    let (p1, log1) = fake_with_in_doubt(&[]);
    let coordinator = TwoPhaseCoordinator::new(db().await, registry_of(&[("p1", p1)]));
    coordinator.ensure_schema().await.unwrap();
    coordinator
        .insert_log("tx-started", "started", &["p1"], None)
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
    assert!(log(&log1).await.contains(&"rollback".to_owned()));
    let row = coordinator.load_log("tx-started").await.unwrap().unwrap();
    assert_eq!(row.status, "rolled_back");
}

#[cfg(feature = "sqlx")]
#[tokio::test]
async fn unsupported_dialect_is_rejected() {
    let client = SqlxClient::connect("sqlite::memory:").await.unwrap();
    match SqlxXaParticipant::new(&client) {
        Err(XaError::UnsupportedDialect) => {}
        Err(other) => panic!("expected UnsupportedDialect, got {other}"),
        Ok(_) => panic!("SQLite must not be accepted as an XA participant"),
    }
}

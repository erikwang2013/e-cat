// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! health 测试：冷层可达性与探针预算；假 sink，不连真存储。
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_data::Row;
use ecat_health::HealthCheck;

use super::TieringHealthCheck;
use crate::{ColdSink, TieringError};

struct FakeSink {
    fail: bool,
    delay: Duration,
}

impl FakeSink {
    fn ok() -> Self {
        Self {
            fail: false,
            delay: Duration::ZERO,
        }
    }
}

#[async_trait]
impl ColdSink for FakeSink {
    async fn write_chunk(
        &self,
        _table: &str,
        _partition: &str,
        _chunk: u64,
        _rows: &[Row],
    ) -> Result<String, TieringError> {
        Err(TieringError::Cold("not used by health tests".into()))
    }

    async fn verify_chunk(
        &self,
        _table: &str,
        _partition: &str,
        _chunk: u64,
        _checksum: &str,
    ) -> Result<bool, TieringError> {
        Err(TieringError::Cold("not used by health tests".into()))
    }

    async fn health_check(&self) -> Result<(), TieringError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if self.fail {
            Err(TieringError::Cold("clickhouse unreachable".into()))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn tiering_health_fails_when_cold_store_unreachable() {
    let sink = FakeSink {
        fail: true,
        ..FakeSink::ok()
    };
    let check = TieringHealthCheck::new(Arc::new(sink), Duration::from_secs(1));

    assert_eq!(check.name(), "data.tiering");
    let err = check.check().await.unwrap_err();
    assert!(err.contains("cold"), "{err}");
}

#[tokio::test]
async fn tiering_health_ok_when_cold_store_answers() {
    let check = TieringHealthCheck::new(Arc::new(FakeSink::ok()), Duration::from_secs(1));

    assert_eq!(check.check().await, Ok(()));
}

#[tokio::test]
async fn tiering_health_fails_when_probe_exceeds_budget() {
    let sink = FakeSink {
        delay: Duration::from_secs(30),
        ..FakeSink::ok()
    };
    let check = TieringHealthCheck::new(Arc::new(sink), Duration::from_millis(20));

    let err = check.check().await.unwrap_err();
    assert!(err.contains("cold sink"), "{err}");
}

fn archived(
    table: &str,
    partition: &str,
    updated_at: time::OffsetDateTime,
) -> crate::ArchiveJobState {
    crate::ArchiveJobState {
        table: table.into(),
        partition: partition.into(),
        chunk: 1,
        low_id: 1,
        high_id: 2,
        rows: 2,
        checksum: Some("checksum".into()),
        phase: crate::ArchivePhase::Copied,
        updated_at,
    }
}

async fn state_store() -> crate::ArchiveStateStore {
    let params = ecat_data_sqlx::PoolParams {
        max_connections: 1,
        ..ecat_data_sqlx::PoolParams::default()
    };
    let db = ecat_data_sqlx::SqlxClient::connect_with_params("sqlite::memory:", &params)
        .await
        .unwrap();
    let store = crate::ArchiveStateStore::new(Arc::new(db));
    store.ensure_schema().await.unwrap();
    store
}

#[tokio::test]
async fn archive_lag_recent_state_is_ok() {
    let store = Arc::new(state_store().await);
    store
        .upsert(&archived("events", "p0", time::OffsetDateTime::now_utc()))
        .await
        .unwrap();
    let check = TieringHealthCheck::new(Arc::new(FakeSink::ok()), Duration::from_secs(1))
        .with_archive_lag(store, time::Duration::hours(1));
    assert_eq!(check.check().await, Ok(()));
}

#[tokio::test]
async fn archive_lag_old_state_is_error() {
    let store = Arc::new(state_store().await);
    store
        .upsert(&archived(
            "events",
            "p0",
            time::OffsetDateTime::now_utc() - time::Duration::hours(2),
        ))
        .await
        .unwrap();
    let check = TieringHealthCheck::new(Arc::new(FakeSink::ok()), Duration::from_secs(1))
        .with_archive_lag(store, time::Duration::hours(1));
    let err = check.check().await.unwrap_err();
    assert!(err.contains("archive lag"), "{err}");
    assert!(err.contains("events"), "{err}");
}

#[tokio::test]
async fn archive_lag_no_state_rows_is_ok() {
    let store = Arc::new(state_store().await);
    let check = TieringHealthCheck::new(Arc::new(FakeSink::ok()), Duration::from_secs(1))
        .with_archive_lag(store, time::Duration::hours(1));
    assert_eq!(check.check().await, Ok(()));
}

struct FailingState;

#[async_trait]
impl ecat_data::SqlExecutor for FailingState {
    async fn execute(&self, _sql: &str) -> Result<u64, ecat_data::RdbmsError> {
        Err(ecat_data::RdbmsError::Database("state down".into()))
    }

    async fn query(&self, _sql: &str) -> Result<Vec<Row>, ecat_data::RdbmsError> {
        Err(ecat_data::RdbmsError::Database("state down".into()))
    }

    fn dialect(&self) -> ecat_data::Dialect {
        ecat_data::Dialect::Sqlite
    }
}

#[tokio::test]
async fn archive_lag_state_query_error_propagates() {
    let store = Arc::new(crate::ArchiveStateStore::new(Arc::new(FailingState)));
    let check = TieringHealthCheck::new(Arc::new(FakeSink::ok()), Duration::from_secs(1))
        .with_archive_lag(store, time::Duration::hours(1));
    let err = check.check().await.unwrap_err();
    assert!(err.contains("archive state"), "{err}");
}

#[tokio::test]
async fn tiering_health_without_archive_lag_keeps_reachability_only() {
    let store = Arc::new(crate::ArchiveStateStore::new(Arc::new(FailingState)));
    let _ = store;
    let check = TieringHealthCheck::new(Arc::new(FakeSink::ok()), Duration::from_secs(1));
    assert_eq!(check.check().await, Ok(()));
}

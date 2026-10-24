// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 状态表与状态机：SQLite 内存库上的回环、幂等 upsert 与迁移拒绝。
use std::sync::Arc;

use ecat_data::{Dialect, RdbmsError, Row, SqlExecutor};
use ecat_data_sqlx::{PoolParams, SqlxClient};
use time::{Duration, OffsetDateTime};

use super::{ArchiveJobState, ArchivePhase, ArchiveStateStore};
use crate::TieringError;

/// 单连接池：`sqlite::memory:` 的库随连接存在，多连接会各看各的空库。
async fn fresh_store() -> ArchiveStateStore {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
        .await
        .unwrap();
    let store = ArchiveStateStore::new(Arc::new(db));
    store.ensure_schema().await.unwrap();
    store
}

fn job(phase: ArchivePhase) -> ArchiveJobState {
    ArchiveJobState {
        table: "events".into(),
        partition: "2026_01".into(),
        chunk: 3,
        low_id: 100,
        high_id: 199,
        rows: 100,
        checksum: None,
        phase,
        updated_at: OffsetDateTime::from_unix_timestamp(1_767_225_600).unwrap(),
    }
}

async fn phase_of(store: &ArchiveStateStore) -> ArchivePhase {
    store
        .load("events", "2026_01", 3)
        .await
        .unwrap()
        .unwrap()
        .phase
}

#[tokio::test]
async fn state_roundtrip_and_idempotent_upsert() {
    let store = fresh_store().await;
    let mut s = job(ArchivePhase::Pending);
    store.upsert(&s).await.unwrap();
    assert_eq!(
        store.load("events", "2026_01", 3).await.unwrap(),
        Some(s.clone())
    );

    s.rows = 42;
    s.checksum = Some("deadbeef".into());
    s.phase = ArchivePhase::Copied;
    s.updated_at += Duration::seconds(60);
    store.upsert(&s).await.unwrap();

    assert_eq!(store.load("events", "2026_01", 3).await.unwrap(), Some(s));
    let count = store
        .db
        .query("SELECT COUNT(*) AS n FROM tier_archive_jobs")
        .await
        .unwrap();
    assert_eq!(
        count[0].get("n"),
        Some(&serde_json::json!(1)),
        "同主键 upsert 不得新增行"
    );
}

#[tokio::test]
async fn state_invalid_transition_is_rejected() {
    let store = fresh_store().await;
    store.upsert(&job(ArchivePhase::Pending)).await.unwrap();

    let err = store
        .advance("events", "2026_01", 3, ArchivePhase::Verified, None)
        .await
        .unwrap_err();
    assert!(matches!(err, TieringError::State(_)), "{err}");
    assert_eq!(phase_of(&store).await, ArchivePhase::Pending);

    // 合法路径上 checksum 可选：Some 写入，None 保留原值。
    store
        .advance("events", "2026_01", 3, ArchivePhase::Copied, Some("sum"))
        .await
        .unwrap();
    store
        .advance("events", "2026_01", 3, ArchivePhase::Verified, None)
        .await
        .unwrap();
    let loaded = store.load("events", "2026_01", 3).await.unwrap().unwrap();
    assert_eq!(loaded.phase, ArchivePhase::Verified);
    assert_eq!(loaded.checksum.as_deref(), Some("sum"));
    assert!(loaded.updated_at > job(ArchivePhase::Pending).updated_at);
}

#[tokio::test]
async fn state_verified_required_before_dropped() {
    let store = fresh_store().await;
    store.upsert(&job(ArchivePhase::Pending)).await.unwrap();

    assert!(
        store
            .advance("events", "2026_01", 3, ArchivePhase::Dropped, None)
            .await
            .is_err(),
        "Pending 不能直接 Dropped"
    );
    assert_eq!(phase_of(&store).await, ArchivePhase::Pending);

    store
        .advance("events", "2026_01", 3, ArchivePhase::Copied, Some("sum"))
        .await
        .unwrap();
    assert!(
        store
            .advance("events", "2026_01", 3, ArchivePhase::Dropped, None)
            .await
            .is_err(),
        "Copied 不能直接 Dropped"
    );
    assert_eq!(phase_of(&store).await, ArchivePhase::Copied);

    store
        .advance("events", "2026_01", 3, ArchivePhase::Verified, None)
        .await
        .unwrap();
    store
        .advance("events", "2026_01", 3, ArchivePhase::Dropped, None)
        .await
        .unwrap();
    assert_eq!(phase_of(&store).await, ArchivePhase::Dropped);
    assert!(
        store
            .advance("events", "2026_01", 3, ArchivePhase::Dropped, None)
            .await
            .is_err(),
        "Dropped 是终态"
    );
}

#[tokio::test]
async fn state_load_missing_returns_none() {
    let store = fresh_store().await;
    assert!(store.load("events", "2026_01", 99).await.unwrap().is_none());
    let err = store
        .advance("events", "2026_01", 99, ArchivePhase::Copied, None)
        .await
        .unwrap_err();
    assert!(matches!(err, TieringError::State(_)), "{err}");
}

#[tokio::test]
async fn state_reset_partition_clears_stale_rows() {
    let store = fresh_store().await;
    store.upsert(&job(ArchivePhase::Verified)).await.unwrap();
    let other = ArchiveJobState {
        partition: "2026_02".into(),
        ..job(ArchivePhase::Verified)
    };
    store.upsert(&other).await.unwrap();

    assert_eq!(store.reset_partition("events", "2026_01").await.unwrap(), 1);
    assert!(store.load("events", "2026_01", 3).await.unwrap().is_none());
    assert!(
        store.load("events", "2026_02", 3).await.unwrap().is_some(),
        "只清目标分区"
    );
}

#[tokio::test]
async fn state_mssql_is_unsupported() {
    struct Mssql;
    #[async_trait::async_trait]
    impl SqlExecutor for Mssql {
        async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
            Ok(0)
        }
        async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            Ok(Vec::new())
        }
        fn dialect(&self) -> Dialect {
            Dialect::Mssql
        }
    }

    let store = ArchiveStateStore::new(Arc::new(Mssql));
    assert!(matches!(
        store.ensure_schema().await,
        Err(TieringError::UnsupportedDialect)
    ));
    assert!(matches!(
        store.upsert(&job(ArchivePhase::Pending)).await,
        Err(TieringError::UnsupportedDialect)
    ));
    assert!(matches!(
        store
            .advance("events", "2026_01", 3, ArchivePhase::Copied, None)
            .await,
        Err(TieringError::UnsupportedDialect)
    ));
}

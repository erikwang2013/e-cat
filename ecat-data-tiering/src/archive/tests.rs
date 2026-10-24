// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 归档主循环：fake hot/cold + SQLite 状态库上的幂等、断点、dry-run、锁与窗口。
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use ecat_data_sqlx::{PoolParams, SqlxClient};
use ecat_lock::{DistributedLock, LockError};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};

mod fixes;
mod window;

use super::{ArchiveJob, ColdSink};
use crate::{
    ArchiveJobState, ArchivePhase, ArchiveStateStore, ColdTarget, TableTieringConfig,
    TieringConfig, TieringError,
};

/// 单连接池：`sqlite::memory:` 的库随连接存在，多连接会各看各的空库。
async fn fresh_db() -> Arc<SqlxClient> {
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

/// 建好状态表并返回「任务用」与「断言用」两个句柄（同一连接池）。
async fn stores() -> (ArchiveStateStore, ArchiveStateStore) {
    let db = fresh_db().await;
    let seed = ArchiveStateStore::new(db.clone());
    seed.ensure_schema().await.unwrap();
    (
        ArchiveStateStore::new(db.clone()),
        ArchiveStateStore::new(db),
    )
}

fn table() -> TableTieringConfig {
    TableTieringConfig {
        name: "events".into(),
        time_col: "ts".into(),
        id_col: "id".into(),
        retention_days: 365,
        target: ColdTarget::Clickhouse,
        filter: None,
    }
}

fn config(batch_size: u32) -> TieringConfig {
    TieringConfig {
        enabled: true,
        hot_days: 180,
        partition_months: 1,
        tables: vec![table()],
        batch_size,
        max_rows_per_sec: None,
        verify_sample: None,
        retain_s3_days: 7,
        dry_run: false,
        window: None,
    }
}

fn event_row(id: i64) -> Row {
    Row::new(
        vec!["id".into(), "ts".into(), "payload".into()],
        vec![json!(id), json!("2025-01-01 00:00:00"), json!("x")],
    )
}

struct FakeHot {
    partitions: Vec<String>,
    hot_rows: i64,
    total_rows: AtomicI64,
    rows: Vec<Row>,
    dropped: Mutex<Vec<String>>,
}

impl FakeHot {
    fn new(partitions: &[&str], rows: Vec<Row>) -> Arc<Self> {
        let total_rows = i64::try_from(rows.len()).unwrap_or(i64::MAX);
        Arc::new(Self {
            partitions: partitions.iter().map(|p| (*p).to_owned()).collect(),
            hot_rows: 0,
            total_rows: AtomicI64::new(total_rows),
            rows,
            dropped: Mutex::new(Vec::new()),
        })
    }

    fn set_total_rows(&self, total: i64) {
        self.total_rows.store(total, Ordering::Relaxed);
    }
}

#[async_trait]
impl SqlExecutor for FakeHot {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.dropped.lock().unwrap().push(sql.to_owned());
        Ok(0)
    }

    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database("use query_with".into()))
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        if sql.contains("information_schema.PARTITIONS") {
            return Ok(self
                .partitions
                .iter()
                .map(|p| Row::new(vec!["partition_name".into()], vec![json!(p)]))
                .collect());
        }
        if sql.contains("COUNT(*) AS hot_rows") {
            return Ok(vec![Row::new(
                vec!["hot_rows".into()],
                vec![json!(self.hot_rows)],
            )]);
        }
        if sql.contains("COUNT(*) AS total_rows") {
            return Ok(vec![Row::new(
                vec!["total_rows".into()],
                vec![json!(self.total_rows.load(Ordering::Relaxed))],
            )]);
        }
        let cursor = params.first().and_then(Value::as_i64).unwrap_or(0);
        let limit = params.get(1).and_then(Value::as_i64).unwrap_or(0);
        let limit = usize::try_from(limit).unwrap_or(0);
        Ok(self
            .rows
            .iter()
            .filter(|r| r.get("id").and_then(Value::as_i64).unwrap_or(0) > cursor)
            .take(limit)
            .cloned()
            .collect())
    }

    fn dialect(&self) -> Dialect {
        Dialect::MySql
    }
}

#[async_trait]
impl RdbmsClient for FakeHot {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Ok(Transaction::new())
    }
}

#[derive(Default)]
struct FakeCold {
    verify_ok: bool,
    writes: Mutex<Vec<(String, String, u64, usize)>>,
    verifies: Mutex<Vec<(String, String, u64, String)>>,
}

impl FakeCold {
    fn new(verify_ok: bool) -> Arc<Self> {
        Arc::new(Self {
            verify_ok,
            ..Self::default()
        })
    }

    fn write_count(&self) -> usize {
        self.writes.lock().unwrap().len()
    }

    fn verify_count(&self) -> usize {
        self.verifies.lock().unwrap().len()
    }
}

#[async_trait]
impl ColdSink for FakeCold {
    async fn write_chunk(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        rows: &[Row],
    ) -> Result<String, TieringError> {
        self.writes.lock().unwrap().push((
            table.to_owned(),
            partition.to_owned(),
            chunk,
            rows.len(),
        ));
        Ok(format!("sum-{table}-{partition}-{chunk}"))
    }

    async fn verify_chunk(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        checksum: &str,
    ) -> Result<bool, TieringError> {
        self.verifies.lock().unwrap().push((
            table.to_owned(),
            partition.to_owned(),
            chunk,
            checksum.to_owned(),
        ));
        Ok(self.verify_ok)
    }

    async fn health_check(&self) -> Result<(), TieringError> {
        Ok(())
    }
}

struct FakeLock {
    held: bool,
    released: Mutex<Vec<String>>,
}

impl FakeLock {
    fn new(held: bool) -> Arc<Self> {
        Arc::new(Self {
            held,
            released: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl DistributedLock for FakeLock {
    async fn acquire(&self, _key: &str, _ttl: StdDuration) -> Result<Option<String>, LockError> {
        Ok((!self.held).then(|| "token".to_owned()))
    }

    async fn release(&self, _key: &str, token: &str) -> Result<(), LockError> {
        self.released.lock().unwrap().push(token.to_owned());
        Ok(())
    }
}

async fn job(
    config: TieringConfig,
    hot: Arc<FakeHot>,
    cold: Arc<FakeCold>,
    lock: Option<Arc<dyn DistributedLock>>,
) -> (ArchiveJob, ArchiveStateStore) {
    let (state, inspect) = stores().await;
    (ArchiveJob::new(hot, cold, state, config, lock), inspect)
}

/// 已 `Verified` 的预置行：`updated_at` 决定是否已过 `retain_s3_days`。
fn verified(chunk: u64, low_id: i64, high_id: i64) -> ArchiveJobState {
    ArchiveJobState {
        table: "events".into(),
        partition: "p2026_01".into(),
        chunk,
        low_id,
        high_id,
        rows: u64::try_from(high_id - low_id + 1).unwrap(),
        checksum: Some(format!("sum-events-p2026_01-{chunk}")),
        phase: ArchivePhase::Verified,
        updated_at: OffsetDateTime::now_utc(),
    }
}

async fn phase_of(state: &ArchiveStateStore, chunk: u64) -> ArchivePhase {
    state
        .load("events", "p2026_01", chunk)
        .await
        .unwrap()
        .unwrap()
        .phase
}

#[tokio::test]
async fn archive_copies_all_chunks_and_verifies() {
    let hot = FakeHot::new(
        &["p2026_01"],
        vec![event_row(1), event_row(2), event_row(3)],
    );
    let cold = FakeCold::new(true);
    let lock = FakeLock::new(false);
    let (job, state) = job(config(1), hot.clone(), cold.clone(), Some(lock.clone())).await;

    let report = job.run_once().await.unwrap();

    assert!(!report.skipped);
    assert!(!report.dry_run);
    assert_eq!(report.chunks, 3);
    assert_eq!(report.rows, 3);
    assert_eq!(report.dropped_partitions, 0);
    for chunk in 0..3 {
        assert_eq!(phase_of(&state, chunk).await, ArchivePhase::Verified);
    }
    assert_eq!(cold.write_count(), 3);
    assert_eq!(cold.verify_count(), 3);
    assert!(
        hot.dropped.lock().unwrap().is_empty(),
        "未过保留期不得 drop"
    );
    assert_eq!(lock.released.lock().unwrap().as_slice(), ["token"]);
}

#[tokio::test]
async fn archive_resumes_after_crash_without_duplicates() {
    let cold = FakeCold::new(true);
    let (job, state) = job(
        config(1),
        FakeHot::new(
            &["p2026_01"],
            vec![event_row(1), event_row(2), event_row(3)],
        ),
        cold.clone(),
        None,
    )
    .await;
    let mut copied = verified(0, 1, 1);
    copied.phase = ArchivePhase::Copied;
    state.upsert(&copied).await.unwrap();

    job.run_once().await.unwrap();

    // chunk 0 只校验不重写；chunk 1、2 新写入。
    let written: Vec<u64> = cold.writes.lock().unwrap().iter().map(|w| w.2).collect();
    let verified: Vec<(u64, String)> = cold
        .verifies
        .lock()
        .unwrap()
        .iter()
        .map(|v| (v.2, v.3.clone()))
        .collect();
    assert_eq!(written, vec![1, 2], "已 Copied 的 chunk 不得重复写入");
    assert_eq!(
        verified[0],
        (0, "sum-events-p2026_01-0".into()),
        "续跑先校验预置的 checksum"
    );
    assert_eq!(phase_of(&state, 0).await, ArchivePhase::Verified);
}

#[tokio::test]
async fn archive_does_not_drop_when_verify_fails() {
    let hot = FakeHot::new(
        &["p2026_01"],
        vec![event_row(1), event_row(2), event_row(3)],
    );
    let (job, state) = job(config(1), hot.clone(), FakeCold::new(false), None).await;

    let report = job.run_once().await.unwrap();

    assert_eq!(report.dropped_partitions, 0);
    assert!(hot.dropped.lock().unwrap().is_empty(), "校验失败不得 drop");
    for chunk in 0..3 {
        assert_eq!(phase_of(&state, chunk).await, ArchivePhase::Copied);
    }
}

#[tokio::test]
async fn archive_dry_run_writes_nothing() {
    let mut config = config(1);
    config.dry_run = true;
    let hot = FakeHot::new(&["p2026_01"], vec![event_row(1), event_row(2)]);
    let cold = FakeCold::new(true);
    let (job, state) = job(config, hot, cold.clone(), None).await;

    let report = job.run_once().await.unwrap();

    assert!(report.dry_run);
    assert!(!report.skipped);
    assert_eq!(report.chunks, 2);
    assert_eq!(report.rows, 2);
    assert_eq!(cold.write_count(), 0);
    assert_eq!(cold.verify_count(), 0);
    assert!(state.load("events", "p2026_01", 0).await.unwrap().is_none());
}

#[tokio::test]
async fn archive_second_instance_skips_when_lock_held() {
    let cold = FakeCold::new(true);
    let (job, state) = job(
        config(1),
        FakeHot::new(&["p2026_01"], vec![event_row(1)]),
        cold.clone(),
        Some(FakeLock::new(true)),
    )
    .await;

    let report = job.run_once().await.unwrap();

    assert!(report.skipped);
    assert_eq!(cold.write_count(), 0);
    assert_eq!(cold.verify_count(), 0);
    assert!(state.load("events", "p2026_01", 0).await.unwrap().is_none());
}

#[tokio::test]
async fn archive_drops_partition_after_retention_and_marks_dropped() {
    let hot = FakeHot::new(&["p2026_01"], Vec::new());
    hot.set_total_rows(4);
    let (job, state) = job(config(2), hot.clone(), FakeCold::new(true), None).await;
    for (chunk, low_id, high_id) in [(0, 1, 2), (1, 3, 4)] {
        let mut job_state = verified(chunk, low_id, high_id);
        job_state.updated_at = OffsetDateTime::now_utc() - Duration::days(30);
        state.upsert(&job_state).await.unwrap();
    }

    let report = job.run_once().await.unwrap();

    assert_eq!(report.dropped_partitions, 1);
    assert_eq!(
        hot.dropped.lock().unwrap().as_slice(),
        ["ALTER TABLE `events` DROP PARTITION `p2026_01`"]
    );
    assert_eq!(phase_of(&state, 0).await, ArchivePhase::Dropped);
    assert_eq!(phase_of(&state, 1).await, ArchivePhase::Dropped);
}

#[tokio::test]
async fn archive_rejects_invalid_partition_ident() {
    let hot = FakeHot::new(&["p2026_01; DROP"], vec![event_row(1)]);
    let cold = FakeCold::new(true);
    let (job, _) = job(config(1), hot.clone(), cold.clone(), None).await;

    let err = job.run_once().await.unwrap_err();

    assert!(matches!(err, TieringError::Config(_)), "{err}");
    assert!(hot.dropped.lock().unwrap().is_empty());
    assert_eq!(cold.write_count(), 0);
}

#[tokio::test]
async fn archive_rejects_invalid_table_ident() {
    let mut config = config(1);
    config.tables[0].name = "events; DROP".into();
    let (job, _) = job(
        config,
        FakeHot::new(&[], Vec::new()),
        FakeCold::new(true),
        None,
    )
    .await;

    let err = job.run_once().await.unwrap_err();

    assert!(matches!(err, TieringError::Config(_)), "{err}");
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 评审修复：限速/单次运行预算的注入时钟，以及 `DROP PARTITION` 前的行数对账。

use std::sync::Mutex;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use time::OffsetDateTime;

use super::*;
use crate::Pacer;
use crate::archive::throttle_delay;

/// 可控时钟：每次 `elapsed()` 先返回当前值再走 `step`，测试无需真实等待。
struct FakePacer {
    elapsed: Mutex<StdDuration>,
    step: StdDuration,
    waits: Mutex<Vec<StdDuration>>,
}

impl FakePacer {
    fn new(step: StdDuration) -> Arc<Self> {
        Arc::new(Self {
            elapsed: Mutex::new(StdDuration::ZERO),
            step,
            waits: Mutex::new(Vec::new()),
        })
    }

    fn waits(&self) -> Vec<StdDuration> {
        self.waits.lock().unwrap().clone()
    }
}

#[async_trait]
impl Pacer for FakePacer {
    fn elapsed(&self) -> StdDuration {
        let mut elapsed = self.elapsed.lock().unwrap();
        let now = *elapsed;
        *elapsed += self.step;
        now
    }

    async fn wait(&self, delay: StdDuration) {
        self.waits.lock().unwrap().push(delay);
    }
}

#[test]
fn throttle_delay_keeps_average_rate_under_limit() {
    let secs = StdDuration::from_secs;
    assert_eq!(throttle_delay(3, 1, StdDuration::ZERO), secs(3));
    assert_eq!(
        throttle_delay(3, 1, StdDuration::from_millis(3_500)),
        StdDuration::ZERO,
        "已落后于目标速率时不再等待"
    );
    assert_eq!(throttle_delay(1, 0, StdDuration::ZERO), StdDuration::ZERO);
}

#[tokio::test]
async fn archive_applies_rate_limit() {
    let mut config = config(1);
    config.max_rows_per_sec = Some(1);
    let pacer = FakePacer::new(StdDuration::ZERO);
    let (job, state) = job(
        config,
        FakeHot::new(
            &["p2026_01"],
            vec![event_row(1), event_row(2), event_row(3)],
        ),
        FakeCold::new(true),
        None,
    )
    .await;

    let report = job.with_pacer(pacer.clone()).run_once().await.unwrap();

    assert_eq!(
        pacer.waits(),
        vec![
            StdDuration::from_secs(1),
            StdDuration::from_secs(2),
            StdDuration::from_secs(3)
        ],
        "每写一行都按 max_rows_per_sec 限速"
    );
    assert_eq!(report.chunks, 3);
    assert_eq!(report.rows, 3);
    for chunk in 0..3 {
        assert_eq!(phase_of(&state, chunk).await, ArchivePhase::Verified);
    }
}

#[tokio::test]
async fn archive_zero_rate_means_unlimited() {
    let mut config = config(1);
    config.max_rows_per_sec = Some(0);
    let pacer = FakePacer::new(StdDuration::ZERO);
    let (job, state) = job(
        config,
        FakeHot::new(&["p2026_01"], vec![event_row(1), event_row(2)]),
        FakeCold::new(true),
        None,
    )
    .await;

    let report = job.with_pacer(pacer.clone()).run_once().await.unwrap();

    assert!(pacer.waits().is_empty(), "0 = 不限速");
    assert_eq!(report.chunks, 2);
    assert_eq!(phase_of(&state, 0).await, ArchivePhase::Verified);
}

#[tokio::test]
async fn archive_stops_at_run_deadline_and_resumes() {
    let db = fresh_db().await;
    let state = ArchiveStateStore::new(db.clone());
    state.ensure_schema().await.unwrap();
    let inspect = ArchiveStateStore::new(db.clone());

    let hot = FakeHot::new(
        &["p2026_01"],
        vec![event_row(1), event_row(2), event_row(3)],
    );
    let cold = FakeCold::new(true);
    // elapsed() 第二次调用即 1s：预算到点，只应跑完 chunk 0。
    let pacer = FakePacer::new(StdDuration::from_secs(1));
    let first = ArchiveJob::new(hot.clone(), cold.clone(), state, config(1), None)
        .with_run_timeout(StdDuration::from_secs(1))
        .with_pacer(pacer.clone());
    let report = first.run_once().await.unwrap();

    assert_eq!(report.chunks, 1);
    assert_eq!(report.dropped_partitions, 0);
    assert_eq!(phase_of(&inspect, 0).await, ArchivePhase::Verified);
    assert!(
        inspect
            .load("events", "p2026_01", 1)
            .await
            .unwrap()
            .is_none()
    );

    // 续跑：同一状态库 + 同一批行；chunk 0 不重复写。
    let cold2 = FakeCold::new(true);
    let second = ArchiveJob::new(
        hot,
        cold2.clone(),
        ArchiveStateStore::new(db),
        config(1),
        None,
    );
    let report = second.run_once().await.unwrap();

    assert_eq!(report.chunks, 2);
    let written: Vec<u64> = cold2.writes.lock().unwrap().iter().map(|w| w.2).collect();
    assert_eq!(written, vec![1, 2]);
    for chunk in 0..3 {
        assert_eq!(phase_of(&inspect, chunk).await, ArchivePhase::Verified);
    }
}

#[tokio::test]
async fn archive_skips_drop_when_partition_row_count_changed() {
    let hot = FakeHot::new(&["p2026_01"], Vec::new());
    // 走查后分区里又多了一行（回填/并发写）：4 = 两个已归档 chunk 的行数之和。
    hot.set_total_rows(5);
    let (job, state) = job(config(2), hot.clone(), FakeCold::new(true), None).await;
    for (chunk, low_id, high_id) in [(0, 1, 2), (1, 3, 4)] {
        let mut job_state = verified(chunk, low_id, high_id);
        job_state.updated_at = OffsetDateTime::now_utc() - time::Duration::days(30);
        state.upsert(&job_state).await.unwrap();
    }

    let report = job.run_once().await.unwrap();

    assert_eq!(report.dropped_partitions, 0);
    assert_eq!(report.skipped_drops, 1);
    assert!(
        hot.dropped.lock().unwrap().is_empty(),
        "行数不一致不得 DROP"
    );
    assert_eq!(phase_of(&state, 0).await, ArchivePhase::Verified);
    assert_eq!(phase_of(&state, 1).await, ArchivePhase::Verified);
}

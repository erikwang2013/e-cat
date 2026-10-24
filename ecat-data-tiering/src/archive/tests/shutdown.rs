// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 停机语义：100ms 内打断轮次间隔，已开始的 chunk 写完才退出。
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use ecat_data::{Row, Shutdown};
use tokio::sync::Notify;

use super::{FakeCold, FakeHot, config, event_row, job, phase_of};
use crate::{ArchivePhase, ColdSink, TieringError};

const BOUND: StdDuration = StdDuration::from_millis(100);

#[tokio::test]
async fn shutdown_cancels_archive_within_100ms() {
    let (job, _) = job(
        config(1),
        FakeHot::new(&[], Vec::new()),
        FakeCold::new(true),
        None,
    )
    .await;
    // 轮次间隔 60s：没有停机就得等满它。
    let (handle, mut shutdown) = Shutdown::new();
    let running = tokio::spawn(async move { job.run_until(&mut shutdown).await });
    tokio::task::yield_now().await;
    handle.shutdown();
    tokio::time::timeout(BOUND, running)
        .await
        .expect("停机后 100ms 内必须返回")
        .expect("run_until 任务 panic")
        .expect("run_until 不应报错");
}

#[tokio::test]
async fn run_until_finishes_current_chunk_before_exit() {
    let hot = FakeHot::new(&["p2026_01"], vec![event_row(1)]);
    let cold = Arc::new(GatedCold::default());
    let (job, state) = job(config(1), hot, cold.clone(), None).await;
    let (handle, mut shutdown) = Shutdown::new();
    let running = tokio::spawn(async move { job.run_until(&mut shutdown).await });

    tokio::time::timeout(StdDuration::from_secs(5), cold.started.notified())
        .await
        .expect("chunk 未开始写入");
    handle.shutdown();
    cold.release.notify_one();

    tokio::time::timeout(StdDuration::from_secs(5), running)
        .await
        .expect("run_until 未在 chunk 完成后退出")
        .expect("run_until 任务 panic")
        .expect("run_until 不应报错");
    assert_eq!(
        cold.writes.lock().unwrap().clone(),
        vec![0],
        "停机不得中断进行中的 chunk"
    );
    assert_eq!(phase_of(&state, 0).await, ArchivePhase::Verified);
}

/// 写入开始即发信号、等放行后才落账：把停机精确卡在 chunk 中间。
#[derive(Default)]
struct GatedCold {
    started: Notify,
    release: Notify,
    writes: Mutex<Vec<u64>>,
}

#[async_trait]
impl ColdSink for GatedCold {
    async fn write_chunk(
        &self,
        _table: &str,
        _partition: &str,
        chunk: u64,
        _rows: &[Row],
    ) -> Result<String, TieringError> {
        self.started.notify_one();
        self.release.notified().await;
        self.writes.lock().unwrap().push(chunk);
        Ok(format!("sum-{chunk}"))
    }

    async fn verify_chunk(
        &self,
        _table: &str,
        _partition: &str,
        _chunk: u64,
        _checksum: &str,
    ) -> Result<bool, TieringError> {
        Ok(true)
    }

    async fn health_check(&self) -> Result<(), TieringError> {
        Ok(())
    }
}

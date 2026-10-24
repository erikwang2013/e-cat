// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 停机信号：幂等取消、重复等待、跨任务唤醒。
use std::time::Duration;

use super::Shutdown;

const BOUND: Duration = Duration::from_millis(100);

#[tokio::test]
async fn shutdown_handle_is_idempotent() {
    let (handle, shutdown) = Shutdown::new();
    handle.shutdown();
    handle.shutdown();
    assert!(shutdown.is_cancelled());
}

#[tokio::test]
async fn worker_cancelled_returns_immediately_after_shutdown() {
    let (handle, mut shutdown) = Shutdown::new();
    handle.shutdown();
    tokio::time::timeout(BOUND, shutdown.cancelled())
        .await
        .expect("已停机时 cancelled() 必须立即返回");
}

#[tokio::test]
async fn worker_cancelled_wakes_waiting_task() {
    let (handle, mut shutdown) = Shutdown::new();
    let waiter = tokio::spawn(async move {
        shutdown.cancelled().await;
        shutdown.is_cancelled()
    });
    handle.shutdown();
    let cancelled = tokio::time::timeout(BOUND, waiter)
        .await
        .expect("停机必须唤醒等待中的任务")
        .expect("waiter 任务 panic");
    assert!(cancelled);
}

#[tokio::test]
async fn worker_cancelled_is_reentrant() {
    let (handle, mut shutdown) = Shutdown::new();
    handle.shutdown();
    tokio::time::timeout(BOUND, shutdown.cancelled())
        .await
        .expect("第一次等待应已就绪");
    tokio::time::timeout(BOUND, shutdown.cancelled())
        .await
        .expect("已停机后再次等待必须立即返回");
}

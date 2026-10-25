// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Worker hook 测试：启动/停机、停机幂等与默认关闭的增量性。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ecat_data::Shutdown;

use super::{WorkerHandle, worker_hooks};
use crate::{App, LifecycleHook};

#[tokio::test]
async fn background_worker_starts_and_stops() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let make = move |mut shutdown: Shutdown| {
        let flag = Arc::clone(&flag);
        async move {
            shutdown.cancelled().await;
            flag.store(true, Ordering::SeqCst);
        }
    };
    let (start, stop, _handle) = worker_hooks(make);
    start.on_start().await.unwrap();
    stop.on_stop().await.unwrap();
    assert!(
        stopped.load(Ordering::SeqCst),
        "worker must observe shutdown and exit"
    );
}

#[tokio::test]
async fn background_worker_with_handle_can_shutdown_and_wait() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let (builder, handle) =
        App::builder().background_worker_with_handle(move |mut shutdown: Shutdown| {
            let flag = Arc::clone(&flag);
            async move {
                shutdown.cancelled().await;
                flag.store(true, Ordering::SeqCst);
            }
        });
    let app = builder.build().unwrap();
    for hook in &app.start_hooks {
        hook.on_start().await.unwrap();
    }

    handle.clone().shutdown();
    handle.wait().await.unwrap();
    assert!(
        stopped.load(Ordering::SeqCst),
        "worker must observe shutdown from the returned handle"
    );

    // 句柄取走任务后，app 的 stop hook 仍可幂等停机。
    for hook in &app.stop_hooks {
        hook.on_stop().await.unwrap();
    }
}

#[test]
fn background_worker_is_additive_default_off() {
    // feature-off 的证明是 Step 4 的 `cargo check -p ecat`（不引入 ecat-data）；
    // 本用例在 worker feature 下同时证明该 builder 方法可用。
    let app = App::builder()
        .background_worker(|_shutdown: Shutdown| async {})
        .build()
        .unwrap();
    drop(app);
}

#[tokio::test]
async fn worker_handle_shutdown_is_idempotent() {
    let (handle, mut shutdown) = WorkerHandle::new();
    let task = tokio::spawn(async move {
        shutdown.cancelled().await;
    });
    handle.set_task(task).await.unwrap();
    handle.shutdown();
    handle.shutdown();
    handle.wait().await.unwrap();
    handle.wait().await.unwrap();
}

#[test]
fn worker_handle_is_send_sync_clone() {
    fn assert_traits<T: Send + Sync + Clone>() {}
    assert_traits::<WorkerHandle>();
}

#[tokio::test]
async fn concurrent_waits_both_observe_worker_completion() {
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let (handle, mut shutdown) = WorkerHandle::new();
    let task = tokio::spawn(async move {
        shutdown.cancelled().await;
        // 停机后仍有收尾工作：早退的 waiter 会在这个窗口里观察到未完成。
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        flag.store(true, Ordering::SeqCst);
    });
    handle.set_task(task).await.unwrap();

    // 三个参与者（两个 waiter + 主任务）同时放行，确保两个 wait 并发进入。
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let spawn_waiter = |handle: WorkerHandle| {
        let barrier = Arc::clone(&barrier);
        let finished = Arc::clone(&finished);
        tokio::spawn(async move {
            barrier.wait().await;
            handle.wait().await.unwrap();
            finished.load(Ordering::SeqCst)
        })
    };
    let first = spawn_waiter(handle.clone());
    let second = spawn_waiter(handle.clone());

    barrier.wait().await;
    handle.shutdown();

    let (first, second) = tokio::join!(first, second);
    assert!(
        first.unwrap() && second.unwrap(),
        "both waiters must return only after the worker finished"
    );
}

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
    handle.set_task(task).unwrap();
    handle.shutdown();
    handle.shutdown();
    handle.wait().await.unwrap();
    handle.wait().await.unwrap();
}

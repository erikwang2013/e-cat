// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 通用后台 worker 生命周期：on_start 启动，on_stop 触发停机并等待退出。
use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ecat_data::{Shutdown, ShutdownHandle};

use crate::{AppBuilder, LifecycleHook};

type HookResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// 单个后台任务的停机柄；`shutdown` 幂等，`wait` 可重复调用。
/// clone 出的句柄共享同一停机信号与任务槽。
#[derive(Clone)]
pub struct WorkerHandle {
    shutdown: Arc<ShutdownHandle>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl WorkerHandle {
    fn new() -> (Self, Shutdown) {
        let (shutdown, receiver) = Shutdown::new();
        (
            Self {
                shutdown: Arc::new(shutdown),
                task: Arc::new(Mutex::new(None)),
            },
            receiver,
        )
    }

    pub fn shutdown(&self) {
        self.shutdown.shutdown();
    }

    /// 等待 worker 退出；任务已完成/已取走时直接返回 Ok。
    pub async fn wait(&self) -> HookResult {
        let task = self
            .task
            .lock()
            .map_err(|_| hook_error("worker task lock poisoned"))?
            .take();
        match task {
            Some(task) => task
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
            None => Ok(()),
        }
    }

    fn set_task(&self, task: tokio::task::JoinHandle<()>) -> HookResult {
        let mut slot = self
            .task
            .lock()
            .map_err(|_| hook_error("worker task lock poisoned"))?;
        if slot.is_some() {
            return Err(hook_error("worker already started"));
        }
        *slot = Some(task);
        Ok(())
    }
}

struct WorkerInner<F> {
    make: Mutex<Option<F>>,
    shutdown_rx: Mutex<Option<Shutdown>>,
    handle: WorkerHandle,
}

struct WorkerHook<F> {
    inner: Arc<WorkerInner<F>>,
}

impl<F> Clone for WorkerHook<F> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[async_trait]
impl<F, Fut> LifecycleHook for WorkerHook<F>
where
    F: FnOnce(Shutdown) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    async fn on_start(&self) -> HookResult {
        let make = self
            .inner
            .make
            .lock()
            .map_err(|_| hook_error("worker make lock poisoned"))?
            .take()
            .ok_or_else(|| hook_error("worker already started"))?;
        let shutdown = self
            .inner
            .shutdown_rx
            .lock()
            .map_err(|_| hook_error("worker shutdown lock poisoned"))?
            .take()
            .ok_or_else(|| hook_error("worker already started"))?;
        let task = tokio::spawn(make(shutdown));
        self.inner.handle.set_task(task)
    }

    async fn on_stop(&self) -> HookResult {
        self.inner.handle.shutdown();
        self.inner.handle.wait().await
    }
}

fn worker_hooks<F, Fut>(make: F) -> (WorkerHook<F>, WorkerHook<F>, WorkerHandle)
where
    F: FnOnce(Shutdown) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (handle, shutdown) = WorkerHandle::new();
    let inner = Arc::new(WorkerInner {
        make: Mutex::new(Some(make)),
        shutdown_rx: Mutex::new(Some(shutdown)),
        handle: handle.clone(),
    });
    (
        WorkerHook {
            inner: Arc::clone(&inner),
        },
        WorkerHook { inner },
        handle,
    )
}

fn hook_error(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(message))
}

impl AppBuilder {
    /// 注册一个后台任务：app 启动时 spawn `make(shutdown)`，停机时先发信号再等待退出。
    /// worker future 自己负责停机时限；这里不另加超时。
    pub fn background_worker<F, Fut>(self, make: F) -> Self
    where
        F: FnOnce(Shutdown) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.background_worker_with_handle(make).0
    }

    /// `background_worker` 的可观测变体：额外返回句柄，调用方可主动
    /// `shutdown()` / `wait()`，不必等整个 app 停机。
    pub fn background_worker_with_handle<F, Fut>(mut self, make: F) -> (Self, WorkerHandle)
    where
        F: FnOnce(Shutdown) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (start, stop, handle) = worker_hooks(make);
        self.start_hooks.push(Box::new(start));
        self.stop_hooks.push(Box::new(stop));
        (self, handle)
    }
}

#[cfg(test)]
mod tests;

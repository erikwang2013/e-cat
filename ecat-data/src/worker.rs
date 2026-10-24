// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 后台任务停机信号：`watch` 通道，发送端可重复触发，接收端可重复等待。
use tokio::sync::watch;

/// 停机信号接收端；`cancelled()` 可重复调用，已停机时立即返回。
pub struct Shutdown {
    rx: watch::Receiver<bool>,
}

/// 停机信号发送端；`shutdown()` 幂等。
pub struct ShutdownHandle {
    tx: watch::Sender<bool>,
}

impl Shutdown {
    /// 创建一对信号端；发送端 Drop 也会唤醒等待方（视为停机）。
    pub fn new() -> (ShutdownHandle, Shutdown) {
        let (tx, rx) = watch::channel(false);
        (ShutdownHandle { tx }, Shutdown { rx })
    }

    /// 等待停机；已停机时立即返回。
    pub async fn cancelled(&mut self) {
        if self.is_cancelled() {
            return;
        }
        // 发送端 Drop 时 `changed()` 返回 Err，同样按停机处理，避免永远挂起。
        let _ = self.rx.changed().await;
    }

    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }
}

impl ShutdownHandle {
    /// 触发停机；重复调用不 panic。
    pub fn shutdown(&self) {
        self.tx.send_replace(true);
    }
}

#[cfg(test)]
mod tests;

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 运行期时钟/等待与限速计算：默认单调时钟 + `tokio::time::sleep`，测试注入假实现。
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;

/// 单次运行的默认预算；到点停止取新 chunk，状态保持可续跑。
pub(crate) const DEFAULT_RUN_TIMEOUT: StdDuration = StdDuration::from_secs(300);

/// 运行期时钟与等待。测试注入假实现，限速/预算用例不依赖真实 sleep。
#[async_trait]
pub trait Pacer: Send + Sync {
    /// 本次运行已过时长（单调时钟）。
    fn elapsed(&self) -> StdDuration;
    /// 按 `max_rows_per_sec` 限速等待。
    async fn wait(&self, delay: StdDuration);
}

pub(crate) struct MonotonicPacer {
    started: Instant,
}

impl Default for MonotonicPacer {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

#[async_trait]
impl Pacer for MonotonicPacer {
    fn elapsed(&self) -> StdDuration {
        self.started.elapsed()
    }

    async fn wait(&self, delay: StdDuration) {
        tokio::time::sleep(delay).await;
    }
}

/// 保持平均速率 ≤ `rows_per_sec` 还应等待的时长；`0` = 不限速。
pub(crate) fn throttle_delay(rows: u64, rows_per_sec: u32, elapsed: StdDuration) -> StdDuration {
    if rows_per_sec == 0 {
        return StdDuration::ZERO;
    }
    let required = StdDuration::from_secs_f64(rows as f64 / f64::from(rows_per_sec));
    required.saturating_sub(elapsed)
}

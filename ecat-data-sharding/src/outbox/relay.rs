// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Outbox relay：租约领取、串行投递、失败指数退避与优雅停机。
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use ecat_data::Shutdown;
use time::Duration;
use tracing::warn;

use super::{OutboxRecord, OutboxStore};
use crate::ShardError;

#[cfg(test)]
mod tests;

/// 默认每批领取条数。
const DEFAULT_BATCH: u32 = 32;
/// 默认租约：单条 handler 超过它仍未完成时，行可被其他 relay 回收。
const DEFAULT_LEASE: Duration = Duration::minutes(1);
/// 默认轮次间隔。
const DEFAULT_INTERVAL: StdDuration = StdDuration::from_secs(5);
/// 首次失败的重试延迟，之后每次翻倍。
const BASE_BACKOFF: Duration = Duration::seconds(1);
/// 退避上限。
const MAX_BACKOFF: Duration = Duration::hours(1);

/// outbox 投递处理器。实现方必须幂等：同一个 `id` 的副作用只能发生一次
/// （租约过期后的重放会把同一条记录再次交到这里）。
#[async_trait]
pub trait OutboxHandler: Send + Sync {
    async fn handle(&self, record: &OutboxRecord) -> Result<(), String>;
}

/// 单次 relay 轮次的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayReport {
    pub claimed: u32,
    pub done: u32,
    pub failed: u32,
}

/// relay 依赖的最小存储面；`OutboxStore` 是唯一生产实现，测试用假存储。
#[async_trait]
trait OutboxOps: Send + Sync {
    async fn claim(&self, limit: u32, lease: Duration) -> Result<Vec<OutboxRecord>, ShardError>;
    async fn mark_done(&self, id: &str) -> Result<(), ShardError>;
    async fn mark_failed(&self, id: &str, retry_after: Duration) -> Result<(), ShardError>;
}

#[async_trait]
impl OutboxOps for OutboxStore {
    async fn claim(&self, limit: u32, lease: Duration) -> Result<Vec<OutboxRecord>, ShardError> {
        OutboxStore::claim(self, limit, lease).await
    }

    async fn mark_done(&self, id: &str) -> Result<(), ShardError> {
        OutboxStore::mark_done(self, id).await
    }

    async fn mark_failed(&self, id: &str, retry_after: Duration) -> Result<(), ShardError> {
        OutboxStore::mark_failed(self, id, retry_after).await
    }
}

/// 串行投递 outbox 的后台循环。
pub struct OutboxRelay {
    store: Arc<dyn OutboxOps>,
    handler: Arc<dyn OutboxHandler>,
    batch: u32,
    lease: Duration,
    interval: StdDuration,
}

impl OutboxRelay {
    pub fn new(store: Arc<OutboxStore>, handler: Arc<dyn OutboxHandler>) -> Self {
        Self {
            store,
            handler,
            batch: DEFAULT_BATCH,
            lease: DEFAULT_LEASE,
            interval: DEFAULT_INTERVAL,
        }
    }

    /// 每轮最多领取的条数。
    pub fn with_batch(mut self, batch: u32) -> Self {
        self.batch = batch;
        self
    }

    /// 领取后的租约时长；超过它仍未完成的行会被回收重投。
    pub fn with_lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// 两轮之间的间隔。
    pub fn with_interval(mut self, interval: StdDuration) -> Self {
        self.interval = interval;
        self
    }

    /// 领取一批并串行投递：成功 `mark_done`，失败 `mark_failed`（按 `attempts`
    /// 指数退避）。handler 失败只记在那条记录上，不中断本轮。
    pub async fn run_once(&self) -> Result<RelayReport, ShardError> {
        let records = self.store.claim(self.batch, self.lease).await?;
        let mut report = RelayReport {
            claimed: u32::try_from(records.len()).unwrap_or(u32::MAX),
            ..RelayReport::default()
        };
        // 首版串行投递；handler 失败只影响这一条，不中断本轮。
        for record in &records {
            match self.handler.handle(record).await {
                Ok(()) => {
                    self.store.mark_done(&record.id).await?;
                    report.done += 1;
                }
                Err(error) => {
                    let retry_after = backoff(record.attempts);
                    warn!(
                        outbox_id = %record.id,
                        attempts = record.attempts,
                        error = %error,
                        "outbox handler failed"
                    );
                    self.store.mark_failed(&record.id, retry_after).await?;
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }

    /// 按 `interval` 循环 `run_once`；停机在两轮之间观察，已开始的一批跑完才返回。
    pub async fn run_until(&self, shutdown: &mut Shutdown) -> Result<(), ShardError> {
        if self.interval.is_zero() {
            return Err(ShardError::Config(
                "outbox relay interval must be > 0".into(),
            ));
        }
        let mut ticker = tokio::time::interval(self.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                // 已停机时不再起新一轮。
                biased;
                _ = shutdown.cancelled() => return Ok(()),
                _ = ticker.tick() => {}
            }
            self.run_once().await?;
        }
    }
}

/// 指数退避：`attempts = 0`（首次失败）退避 `BASE_BACKOFF`，之后翻倍，封顶 `MAX_BACKOFF`。
fn backoff(attempts: u32) -> Duration {
    let factor = 1i32 << attempts.min(20);
    BASE_BACKOFF
        .checked_mul(factor)
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF)
}

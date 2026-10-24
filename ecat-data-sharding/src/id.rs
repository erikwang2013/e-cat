// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Snowflake 全局 ID：41 位毫秒时间戳 + 10 位 worker id + 12 位序列。
use std::sync::atomic::{AtomicU64, Ordering};

use time::OffsetDateTime;

use crate::ShardError;

#[cfg(test)]
mod tests;

/// 10 位 worker id：取值范围 0..=1023。
const WORKER_BITS: u32 = 10;
const MAX_WORKER_ID: u64 = (1 << WORKER_BITS) - 1;
/// 12 位序列：单毫秒最多 4096 个 id。
const SEQUENCE_BITS: u32 = 12;
const MAX_SEQUENCE: u64 = (1 << SEQUENCE_BITS) - 1;
const MAX_TIMESTAMP: i64 = (1 << 41) - 1;
/// `state` 未使用时的哨兵；合法打包状态最高约 2^53，永远到不了 `u64::MAX`。
const STATE_UNSET: u64 = u64::MAX;

/// Snowflake 生成器；`next_id` 线程安全，时钟可注入以便测试。
pub struct Snowflake {
    worker_id: u64,
    epoch_ms: i64,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    /// `(相对毫秒 << 12) | 最近发出的序列`：毫秒与序列必须由同一个原子发布，
    /// 否则换毫秒时别的线程可能读到尚未重置的序列而重号。
    state: AtomicU64,
}

impl Snowflake {
    /// 用系统时钟构造。
    pub fn new(worker_id: u64, epoch: OffsetDateTime) -> Result<Self, ShardError> {
        Self::with_clock(worker_id, epoch, system_clock_ms)
    }

    /// 注入返回 Unix 毫秒的时钟；`epoch` 是时间戳位的零点。
    pub fn with_clock(
        worker_id: u64,
        epoch: OffsetDateTime,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Result<Self, ShardError> {
        if worker_id > MAX_WORKER_ID {
            return Err(ShardError::IdOutOfRange);
        }
        let epoch_ms = i64::try_from(epoch.unix_timestamp_nanos().div_euclid(1_000_000))
            .map_err(|_| ShardError::IdOutOfRange)?;
        Ok(Self {
            worker_id,
            epoch_ms,
            clock: Box::new(clock),
            state: AtomicU64::new(STATE_UNSET),
        })
    }

    /// 生成下一个全局唯一 id；时钟回拨报错，不回退等待。
    pub fn next_id(&self) -> Result<i64, ShardError> {
        loop {
            // 先读状态再读时钟：状态只可能由更早的时钟读数发布（acquire 保证时钟调用
            // 不会被上提到 load 之前），因此下面的回拨判断不会把并发推进误判成回拨。
            let state = self.state.load(Ordering::Acquire);
            let now = (self.clock)() - self.epoch_ms;
            let last_ms = if state == STATE_UNSET {
                None
            } else {
                Some((state >> SEQUENCE_BITS) as i64)
            };
            if last_ms.is_some_and(|last| now < last) {
                return Err(ShardError::ClockRollback);
            }
            if !(0..=MAX_TIMESTAMP).contains(&now) {
                return Err(ShardError::IdOutOfRange);
            }
            let sequence = match last_ms {
                Some(last) if now == last => {
                    let sequence = state & MAX_SEQUENCE;
                    if sequence == MAX_SEQUENCE {
                        // 本毫秒序列耗尽：自旋到下一毫秒（注入不前进的时钟会一直自旋）。
                        continue;
                    }
                    sequence + 1
                }
                _ => 0,
            };
            let next = ((now as u64) << SEQUENCE_BITS) | sequence;
            if self
                .state
                .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(compose_id(now as u64, self.worker_id, sequence));
            }
        }
    }
}

fn compose_id(timestamp: u64, worker_id: u64, sequence: u64) -> i64 {
    ((timestamp << (WORKER_BITS + SEQUENCE_BITS)) | (worker_id << SEQUENCE_BITS) | sequence) as i64
}

fn system_clock_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

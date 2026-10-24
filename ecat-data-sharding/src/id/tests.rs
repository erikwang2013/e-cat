// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Snowflake 测试：递增、并发唯一、回拨拒绝、worker 越界、同毫秒序列耗尽。
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use time::OffsetDateTime;

use crate::{ShardError, Snowflake};

const WORKER: u64 = 7;
const EPOCH_SECONDS: i64 = 1_700_000_000;
const EPOCH_MS: i64 = EPOCH_SECONDS * 1_000;
const BASE_MS: i64 = EPOCH_MS + 12_345;

fn epoch() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(EPOCH_SECONDS).unwrap()
}

fn snowflake_with(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Snowflake {
    Snowflake::with_clock(WORKER, epoch(), clock).unwrap()
}

#[test]
fn snowflake_ids_are_strictly_increasing() {
    // 每次读时钟前进 1ms：5000 个 id 每个占一个新毫秒。
    let ticks = AtomicI64::new(BASE_MS);
    let snowflake = snowflake_with(move || ticks.fetch_add(1, Ordering::Relaxed));

    let ids: Vec<i64> = (0..5000).map(|_| snowflake.next_id().unwrap()).collect();

    assert!(ids.windows(2).all(|pair| pair[1] > pair[0]));
    // 位布局：41 位时间戳 + 10 位 worker + 12 位序列。
    assert_eq!(ids[0] >> 22, BASE_MS - EPOCH_MS);
    assert!(ids.iter().all(|id| (id >> 12) & 0x3FF == WORKER as i64));
    assert!(ids.iter().all(|id| id & 0xFFF == 0));
}

#[test]
fn snowflake_ids_are_unique_under_concurrency() {
    // 每 4096 次时钟调用推进 1ms：同毫秒内会用到序列位，且序列耗尽后能自旋到下一毫秒。
    let calls = Arc::new(AtomicI64::new(0));
    let snowflake = Arc::new(snowflake_with({
        let calls = Arc::clone(&calls);
        move || BASE_MS + calls.fetch_add(1, Ordering::Relaxed) / 4096
    }));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let snowflake = Arc::clone(&snowflake);
            std::thread::spawn(move || {
                (0..1000)
                    .map(|_| snowflake.next_id().unwrap())
                    .collect::<Vec<i64>>()
            })
        })
        .collect();

    let mut ids = HashSet::new();
    for handle in handles {
        ids.extend(handle.join().unwrap());
    }
    assert_eq!(ids.len(), 8000);
}

#[test]
fn snowflake_clock_rollback_is_rejected() {
    let now = Arc::new(AtomicI64::new(BASE_MS));
    let snowflake = snowflake_with({
        let now = Arc::clone(&now);
        move || now.load(Ordering::Relaxed)
    });

    assert!(snowflake.next_id().is_ok());
    now.store(BASE_MS - 1, Ordering::Relaxed);
    assert!(matches!(
        snowflake.next_id(),
        Err(ShardError::ClockRollback)
    ));
}

#[test]
fn snowflake_worker_id_out_of_range_is_rejected() {
    assert!(matches!(
        Snowflake::new(1024, epoch()),
        Err(ShardError::IdOutOfRange)
    ));
    assert!(matches!(
        Snowflake::with_clock(1024, epoch(), || BASE_MS),
        Err(ShardError::IdOutOfRange)
    ));
    assert!(Snowflake::new(1023, epoch()).is_ok());
}

#[test]
fn snowflake_sequence_overflow_waits_for_next_millisecond() {
    // 前 5000 次时钟调用都落在同一毫秒：4096 个序列用完后必须自旋到下一毫秒。
    let calls = AtomicI64::new(0);
    let snowflake = snowflake_with(move || BASE_MS + calls.fetch_add(1, Ordering::Relaxed) / 5000);

    let ids: Vec<i64> = (0..5000).map(|_| snowflake.next_id().unwrap()).collect();

    let unique: HashSet<i64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len());
    assert!(ids.windows(2).all(|pair| pair[1] > pair[0]));
    // 第 4097 个 id（下标 4096）落在下一毫秒。
    assert_eq!(ids[4096] >> 22, BASE_MS - EPOCH_MS + 1);
}

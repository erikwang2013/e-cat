// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! relay 测试：假存储 + 假 handler 覆盖投递、退避与停机语义（不连真实数据库）。
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use ecat_data::Shutdown;
use time::{Duration, OffsetDateTime};
use tokio::sync::Notify;

use super::{MAX_BACKOFF, OutboxHandler, OutboxOps, OutboxRelay, RelayReport, backoff};
use crate::ShardError;
use crate::outbox::{OutboxRecord, OutboxStatus};

fn record(id: &str, attempts: u32) -> OutboxRecord {
    OutboxRecord {
        id: id.into(),
        aggregate: "order".into(),
        payload: format!("payload-{id}").into_bytes(),
        status: OutboxStatus::Processing,
        attempts,
        next_attempt_at: OffsetDateTime::now_utc(),
    }
}

/// 假存储：队列由用例预置，`mark_*` 只记录调用（不动时间、不依赖 SQL）。
#[derive(Default)]
struct FakeStore {
    queue: Mutex<VecDeque<OutboxRecord>>,
    claims: AtomicU32,
    done: Mutex<Vec<String>>,
    failed: Mutex<Vec<(String, Duration)>>,
}

impl FakeStore {
    fn push(&self, record: OutboxRecord) {
        self.queue.lock().unwrap().push_back(record);
    }

    fn done(&self) -> Vec<String> {
        self.done.lock().unwrap().clone()
    }

    fn failed(&self) -> Vec<(String, Duration)> {
        self.failed.lock().unwrap().clone()
    }

    fn claim_count(&self) -> u32 {
        self.claims.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl OutboxOps for FakeStore {
    async fn claim(&self, limit: u32, _lease: Duration) -> Result<Vec<OutboxRecord>, ShardError> {
        self.claims.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.queue.lock().unwrap();
        let n = (limit as usize).min(queue.len());
        Ok(queue.drain(..n).collect())
    }

    async fn mark_done(&self, id: &str) -> Result<(), ShardError> {
        self.done.lock().unwrap().push(id.to_owned());
        Ok(())
    }

    async fn mark_failed(&self, id: &str, retry_after: Duration) -> Result<(), ShardError> {
        self.failed
            .lock()
            .unwrap()
            .push((id.to_owned(), retry_after));
        Ok(())
    }
}

fn relay(store: &Arc<FakeStore>, handler: Arc<dyn OutboxHandler>) -> OutboxRelay {
    OutboxRelay {
        store: store.clone(),
        handler,
        batch: 10,
        lease: Duration::minutes(1),
        interval: StdDuration::from_millis(10),
    }
}

struct OkHandler;

#[async_trait]
impl OutboxHandler for OkHandler {
    async fn handle(&self, _record: &OutboxRecord) -> Result<(), String> {
        Ok(())
    }
}

/// 只让 `a` 失败：同时证明失败条目不阻断同批后续记录。
struct FailAHandler;

#[async_trait]
impl OutboxHandler for FailAHandler {
    async fn handle(&self, record: &OutboxRecord) -> Result<(), String> {
        if record.id == "a" {
            return Err("boom a".into());
        }
        Ok(())
    }
}

/// 第一次投递卡住（模拟停机落在批中间），之后立刻成功。
#[derive(Default)]
struct GatedHandler {
    started: Notify,
    release: Notify,
    calls: AtomicU32,
}

#[async_trait]
impl OutboxHandler for GatedHandler {
    async fn handle(&self, _record: &OutboxRecord) -> Result<(), String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// 幂等 handler：按 `id` 去重，同一个 id 只产生一次副作用。
#[derive(Default)]
struct DedupHandler {
    applied: Mutex<HashSet<String>>,
    effects: AtomicU32,
}

#[async_trait]
impl OutboxHandler for DedupHandler {
    async fn handle(&self, record: &OutboxRecord) -> Result<(), String> {
        if self.applied.lock().unwrap().insert(record.id.clone()) {
            self.effects.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[tokio::test]
async fn relay_rejects_zero_batch_and_lease() {
    let store = Arc::new(FakeStore::default());
    store.push(record("a", 0));

    // 无效配置必须在 claim 之前报错：否则 batch = 0 会静默空转，
    // lease = 0 会让整批刚领到的记录立即可被回收。
    let zero_batch = relay(&store, Arc::new(OkHandler)).with_batch(0);
    assert!(matches!(
        zero_batch.run_once().await,
        Err(ShardError::Config(_))
    ));
    let zero_lease = relay(&store, Arc::new(OkHandler)).with_lease(Duration::ZERO);
    assert!(matches!(
        zero_lease.run_once().await,
        Err(ShardError::Config(_))
    ));

    assert_eq!(store.claim_count(), 0, "无效配置不得领取记录");
    assert!(store.done().is_empty());
}

#[tokio::test]
async fn relay_rejects_zero_interval() {
    let store = Arc::new(FakeStore::default());
    store.push(record("a", 0));
    let (_handle, mut shutdown) = Shutdown::new();
    let zero_interval = relay(&store, Arc::new(OkHandler)).with_interval(StdDuration::ZERO);

    // 不先校验的话 tokio::time::interval(0) 会 panic。
    assert!(matches!(
        zero_interval.run_until(&mut shutdown).await,
        Err(ShardError::Config(_))
    ));
    assert_eq!(store.claim_count(), 0, "无效配置不得领取记录");
}

#[test]
fn relay_backoff_saturates_at_max() {
    // 上限以下仍按 2 的幂增长：2^10s、2^11s。
    assert_eq!(backoff(10), Duration::seconds(1024));
    assert_eq!(backoff(11), Duration::seconds(2048));
    // 2^12s = 4096s 已超过 1h 上限，故 attempts >= 12 恒定 1h。
    assert_eq!(backoff(12), MAX_BACKOFF);
    assert_eq!(backoff(13), MAX_BACKOFF);
    assert_eq!(backoff(20), MAX_BACKOFF);
    assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
}

#[tokio::test]
async fn relay_marks_done_on_success() {
    let store = Arc::new(FakeStore::default());
    store.push(record("a", 0));
    store.push(record("b", 0));
    let relay = relay(&store, Arc::new(OkHandler));

    let report = relay.run_once().await.unwrap();
    assert_eq!(
        report,
        RelayReport {
            claimed: 2,
            done: 2,
            failed: 0
        }
    );
    assert_eq!(store.done(), ["a", "b"]);
    assert!(store.failed().is_empty());

    // 队列已空：下一轮 claim 到 0 条。
    assert_eq!(relay.run_once().await.unwrap(), RelayReport::default());
    assert_eq!(store.claim_count(), 2);
}

#[tokio::test]
async fn relay_marks_failed_and_backs_off() {
    let store = Arc::new(FakeStore::default());
    store.push(record("a", 0));
    store.push(record("b", 0));
    let relay = relay(&store, Arc::new(FailAHandler));

    let report = relay.run_once().await.unwrap();
    assert_eq!(
        report,
        RelayReport {
            claimed: 2,
            done: 1,
            failed: 1
        }
    );
    // 失败条目不阻断同批后续记录。
    assert_eq!(store.done(), ["b"]);
    // 首次失败（attempts = 0）退避 1s，由存储层写进 next_attempt_at。
    assert_eq!(store.failed(), [("a".to_owned(), Duration::seconds(1))]);

    // 存储层回收/重投后 attempts 已累加：第二次失败退避翻倍。
    store.push(record("a", 1));
    let report = relay.run_once().await.unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(
        store.failed(),
        [
            ("a".to_owned(), Duration::seconds(1)),
            ("a".to_owned(), Duration::seconds(2)),
        ]
    );
}

#[tokio::test]
async fn relay_shutdown_stops_after_current_batch() {
    let store = Arc::new(FakeStore::default());
    for id in ["a", "b", "c"] {
        store.push(record(id, 0));
    }
    let handler = Arc::new(GatedHandler::default());
    // 轮次间隔 60s：没有停机就会一直挂着；停机必须由 select 立即观察。
    let relay = relay(&store, handler.clone()).with_interval(StdDuration::from_secs(60));
    let (handle, mut shutdown) = Shutdown::new();
    let running = tokio::spawn(async move { relay.run_until(&mut shutdown).await });

    tokio::time::timeout(StdDuration::from_secs(5), handler.started.notified())
        .await
        .expect("handler 未被调用");
    handle.shutdown();
    handler.release.notify_one();

    tokio::time::timeout(StdDuration::from_secs(5), running)
        .await
        .expect("停机未在当前批完成后退出")
        .expect("run_until 任务 panic")
        .expect("run_until 不应报错");
    assert_eq!(
        handler.calls.load(Ordering::SeqCst),
        3,
        "当前批必须全部投递完"
    );
    assert_eq!(store.done(), ["a", "b", "c"]);
    assert_eq!(store.claim_count(), 1, "停机后不得再起新一轮");
}

#[tokio::test]
async fn relay_duplicate_delivery_is_idempotent() {
    let store = Arc::new(FakeStore::default());
    store.push(record("a", 0));
    // 租约回收后的重放：同一条记录（同 id）被再次投递。
    store.push(record("a", 0));
    let handler = Arc::new(DedupHandler::default());
    let relay = relay(&store, handler.clone());

    let report = relay.run_once().await.unwrap();
    assert_eq!(
        report,
        RelayReport {
            claimed: 2,
            done: 2,
            failed: 0
        }
    );
    assert_eq!(
        handler.effects.load(Ordering::SeqCst),
        1,
        "同一个 id 的副作用只能发生一次"
    );
}

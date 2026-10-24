// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! outbox 存储测试：SQLite 内存库上的领取、租约回收、幂等完成与退避重试。
use std::sync::Arc;

use ecat_data_sqlx::{PoolParams, SqlxClient};
use time::{Duration, OffsetDateTime};

use super::{OutboxRecord, OutboxStatus, OutboxStore, fmt_ts, parse_ts};

/// 单连接池：`sqlite::memory:` 的库随连接存在，多连接会各看各的空库。
async fn fresh_store() -> OutboxStore {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
        .await
        .unwrap();
    let store = OutboxStore::new(Arc::new(db));
    store.ensure_schema().await.unwrap();
    store
}

fn record(id: &str) -> OutboxRecord {
    OutboxRecord {
        id: id.into(),
        aggregate: "order".into(),
        payload: format!("payload-{id}").into_bytes(),
        status: OutboxStatus::Pending,
        attempts: 0,
        next_attempt_at: OffsetDateTime::now_utc() - Duration::seconds(1),
    }
}

fn ids(records: &[OutboxRecord]) -> Vec<&str> {
    records.iter().map(|r| r.id.as_str()).collect()
}

async fn status_and_attempts(store: &OutboxStore, id: &str) -> (String, i64) {
    let rows = store
        .db
        .query_with(
            "SELECT status, attempts FROM ecat_outbox WHERE id = ?",
            &[serde_json::json!(id)],
        )
        .await
        .unwrap();
    let row = rows.first().unwrap();
    (
        row.get("status").unwrap().as_str().unwrap().to_owned(),
        row.get("attempts").unwrap().as_i64().unwrap(),
    )
}

async fn set_next_attempt(store: &OutboxStore, id: &str, at: OffsetDateTime) {
    store
        .db
        .execute_with(
            "UPDATE ecat_outbox SET next_attempt_at = ? WHERE id = ?",
            &[
                serde_json::json!(fmt_ts(at).unwrap()),
                serde_json::json!(id),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn outbox_append_then_claim_returns_pending() {
    let store = fresh_store().await;
    // 重复建表不报错（ensure_schema 幂等）。
    store.ensure_schema().await.unwrap();
    let rec = record("a");
    store.append(&rec).await.unwrap();

    let claimed = store.claim(10, Duration::minutes(1)).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, "a");
    assert_eq!(claimed[0].aggregate, "order");
    assert_eq!(claimed[0].payload, rec.payload);
    // claim 返回的是已领取的记录：调用方此刻持有租约。
    assert_eq!(claimed[0].status, OutboxStatus::Processing);
    assert_eq!(claimed[0].attempts, 0);

    // 租约未过期，同一条记录不会被再次领取。
    assert!(
        store
            .claim(10, Duration::minutes(1))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn outbox_claim_skips_future_retry() {
    let store = fresh_store().await;
    let mut later = record("later");
    later.next_attempt_at = OffsetDateTime::now_utc() + Duration::hours(1);
    store.append(&record("ready")).await.unwrap();
    store.append(&later).await.unwrap();

    assert_eq!(
        ids(&store.claim(10, Duration::minutes(1)).await.unwrap()),
        ["ready"]
    );
}

#[tokio::test]
async fn outbox_concurrent_claims_are_disjoint() {
    let store = fresh_store().await;
    for id in ["a", "b", "c", "d"] {
        store.append(&record(id)).await.unwrap();
    }
    // Barrier 让两个 claim 同时起跑；单连接池把它们串行化，
    // 结果只取决于事务是否原子，不依赖 sleep 或计时。
    let barrier = tokio::sync::Barrier::new(2);
    let (first, second) = tokio::join!(
        async {
            barrier.wait().await;
            store.claim(2, Duration::minutes(1)).await.unwrap()
        },
        async {
            barrier.wait().await;
            store.claim(2, Duration::minutes(1)).await.unwrap()
        },
    );

    let first: std::collections::HashSet<_> = ids(&first).into_iter().collect();
    let second: std::collections::HashSet<_> = ids(&second).into_iter().collect();
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert!(first.is_disjoint(&second), "{first:?} overlaps {second:?}");
    assert_eq!(first.union(&second).count(), 4);
}

#[tokio::test]
async fn outbox_stale_processing_is_reclaimed() {
    let store = fresh_store().await;
    store.append(&record("a")).await.unwrap();
    assert_eq!(
        ids(&store.claim(10, Duration::hours(1)).await.unwrap()),
        ["a"]
    );

    // 租约内：不能被抢走。
    assert!(
        store
            .claim(10, Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );

    // 把 claimed_at 拨老两小时（不 sleep），超过租约后回收。handler 崩溃时走不到
    // mark_failed，回收必须自己累加 attempts 并退避，不能立刻原样重投。
    let old = OffsetDateTime::now_utc() - Duration::hours(2);
    store
        .db
        .execute_with(
            "UPDATE ecat_outbox SET claimed_at = ? WHERE id = ?",
            &[
                serde_json::json!(fmt_ts(old).unwrap()),
                serde_json::json!("a"),
            ],
        )
        .await
        .unwrap();
    assert!(
        store
            .claim(10, Duration::hours(1))
            .await
            .unwrap()
            .is_empty(),
        "回收后应在退避窗口内，不能立刻重投"
    );
    assert_eq!(
        status_and_attempts(&store, "a").await,
        ("pending".to_owned(), 1)
    );

    // 退避到期后可再次领取，且 attempts 保留。
    set_next_attempt(
        &store,
        "a",
        OffsetDateTime::now_utc() - Duration::seconds(1),
    )
    .await;
    let reclaimed = store.claim(10, Duration::hours(1)).await.unwrap();
    assert_eq!(ids(&reclaimed), ["a"]);
    assert_eq!(reclaimed[0].attempts, 1);

    // 第二次崩溃：attempts 随每次回收继续增长（毒消息不会停在 0）。
    let old = OffsetDateTime::now_utc() - Duration::hours(2);
    store
        .db
        .execute_with(
            "UPDATE ecat_outbox SET claimed_at = ? WHERE id = ?",
            &[
                serde_json::json!(fmt_ts(old).unwrap()),
                serde_json::json!("a"),
            ],
        )
        .await
        .unwrap();
    assert!(
        store
            .claim(10, Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        status_and_attempts(&store, "a").await,
        ("pending".to_owned(), 2)
    );
}

#[tokio::test]
async fn outbox_mark_done_is_idempotent() {
    let store = fresh_store().await;
    store.append(&record("a")).await.unwrap();
    store.claim(10, Duration::minutes(1)).await.unwrap();

    store.mark_done("a").await.unwrap();
    store.mark_done("a").await.unwrap();
    assert_eq!(status_and_attempts(&store, "a").await.0, "done");
    assert!(
        store
            .claim(10, Duration::minutes(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn outbox_mark_failed_increments_attempts_and_sets_next_attempt() {
    let store = fresh_store().await;
    store.append(&record("a")).await.unwrap();
    store.claim(10, Duration::minutes(1)).await.unwrap();

    store.mark_failed("a", Duration::minutes(5)).await.unwrap();
    let (status, attempts) = status_and_attempts(&store, "a").await;
    assert_eq!(status, "failed");
    assert_eq!(attempts, 1);

    let rows = store
        .db
        .query_with(
            "SELECT next_attempt_at FROM ecat_outbox WHERE id = ?",
            &[serde_json::json!("a")],
        )
        .await
        .unwrap();
    let next = parse_ts(rows[0].get("next_attempt_at").unwrap().as_str().unwrap()).unwrap();
    let now = OffsetDateTime::now_utc();
    assert!(next > now && next <= now + Duration::minutes(6), "{next}");

    // 退避未到：不可领取。
    assert!(
        store
            .claim(10, Duration::minutes(1))
            .await
            .unwrap()
            .is_empty()
    );

    // mark_failed 只对 Processing 生效：先重新领取，再失败一次才能累加 attempts。
    set_next_attempt(
        &store,
        "a",
        OffsetDateTime::now_utc() - Duration::seconds(1),
    )
    .await;
    let retried = store.claim(10, Duration::minutes(1)).await.unwrap();
    assert_eq!(ids(&retried), ["a"]);
    assert_eq!(retried[0].attempts, 1);

    store.mark_failed("a", Duration::minutes(5)).await.unwrap();
    assert_eq!(
        status_and_attempts(&store, "a").await,
        ("failed".to_owned(), 2)
    );

    // 再到期后可领取，attempts 保留。
    set_next_attempt(
        &store,
        "a",
        OffsetDateTime::now_utc() - Duration::seconds(1),
    )
    .await;
    let retried = store.claim(10, Duration::minutes(1)).await.unwrap();
    assert_eq!(ids(&retried), ["a"]);
    assert_eq!(retried[0].attempts, 2);
    assert_eq!(retried[0].status, OutboxStatus::Processing);
}

#[tokio::test]
async fn outbox_mark_failed_does_not_revive_done() {
    let store = fresh_store().await;
    store.append(&record("a")).await.unwrap();
    store.claim(10, Duration::minutes(1)).await.unwrap();
    store.mark_done("a").await.unwrap();

    // 迟到的失败上报（租约过期后才回来）不能把已完成的工作翻回 Failed 重新入队。
    store.mark_failed("a", Duration::ZERO).await.unwrap();
    assert_eq!(
        status_and_attempts(&store, "a").await,
        ("done".to_owned(), 0)
    );
    assert!(
        store
            .claim(10, Duration::minutes(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn outbox_append_rejects_non_pending() {
    let store = fresh_store().await;
    for status in [
        OutboxStatus::Processing,
        OutboxStatus::Failed,
        OutboxStatus::Done,
    ] {
        let mut rec = record("a");
        rec.status = status;
        assert!(
            matches!(store.append(&rec).await, Err(crate::ShardError::Config(_))),
            "append must reject {status:?}"
        );
    }
    // 拒绝的三条一条都没写进去。
    assert_eq!(store.pending_count().await.unwrap(), 0);

    // 对照组：Pending 正常写入（否则上面的断言只是证明了 append 永远失败）。
    store.append(&record("ok")).await.unwrap();
    assert_eq!(store.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn outbox_pending_count_counts_not_done() {
    let store = fresh_store().await;
    for id in ["a", "b", "c", "d"] {
        store.append(&record(id)).await.unwrap();
    }

    // 造出四种状态各一行：1 条留在 Pending，其余依次领取/完成/失败。
    store.claim(1, Duration::minutes(1)).await.unwrap();
    let done = store.claim(1, Duration::minutes(1)).await.unwrap();
    store.mark_done(&done[0].id).await.unwrap();
    let failed = store.claim(1, Duration::minutes(1)).await.unwrap();
    store
        .mark_failed(&failed[0].id, Duration::minutes(5))
        .await
        .unwrap();

    let rows = store
        .db
        .query("SELECT status, COUNT(*) AS n FROM ecat_outbox GROUP BY status")
        .await
        .unwrap();
    let mut counts = std::collections::BTreeMap::new();
    for row in &rows {
        counts.insert(
            row.get("status").unwrap().as_str().unwrap().to_owned(),
            row.get("n").unwrap().as_i64().unwrap(),
        );
    }
    assert_eq!(
        counts,
        std::collections::BTreeMap::from([
            ("pending".to_owned(), 1),
            ("processing".to_owned(), 1),
            ("failed".to_owned(), 1),
            ("done".to_owned(), 1),
        ]),
        "setup must produce one row of each status"
    );
    // 积压 = 非 Done：Pending + Processing + Failed；Done 不计。
    assert_eq!(store.pending_count().await.unwrap(), 3);
}

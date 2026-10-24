// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! scatter 测试：同一条 SQL 扇出到全部分片、归并排序/去重/分页、
//! 分片失败与总超时整体报错、并发上限。
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::ScatterRequest;
use crate::{ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardRouter, ShardTable};

const SHARDS: [&str; 4] = ["s0", "s1", "s2", "s3"];
const SQL: &str = "SELECT id, shard FROM users";

/// 一句 `query_with` 的调用记录：证明 SQL/参数原样下发、每个分片各一次。
type Calls = Arc<Mutex<Vec<(String, Vec<Value>)>>>;

/// 「同时在飞」的观测点：0 个许可，由测试显式放行。
struct Gate {
    in_flight: AtomicUsize,
    seen_max: AtomicUsize,
    permits: Semaphore,
}

/// 分片行为。
enum Behavior {
    Rows(Vec<Row>),
    Fail,
    Sleep(Duration),
    Gated { gate: Arc<Gate>, rows: Vec<Row> },
}

struct FakeClient {
    behavior: Behavior,
    calls: Calls,
}

#[async_trait]
impl SqlExecutor for FakeClient {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database(
            "scatter tests only use query_with".into(),
        ))
    }
    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(
            "scatter tests only use query_with".into(),
        ))
    }
    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.calls
            .lock()
            .unwrap()
            .push((sql.to_owned(), params.to_vec()));
        match &self.behavior {
            Behavior::Rows(rows) => Ok(rows.clone()),
            Behavior::Fail => Err(RdbmsError::Database("shard down".into())),
            Behavior::Sleep(delay) => {
                tokio::time::sleep(*delay).await;
                Ok(Vec::new())
            }
            Behavior::Gated { gate, rows } => {
                let now = gate.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                gate.seen_max.fetch_max(now, Ordering::SeqCst);
                let _permit = gate.permits.acquire().await.expect("gate closed");
                gate.in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(rows.clone())
            }
        }
    }
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }
}

#[async_trait]
impl RdbmsClient for FakeClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "scatter tests do not use transactions".into(),
        ))
    }
}

fn table() -> ShardTable {
    ShardTable {
        name: "users".to_owned(),
        key: "id".to_owned(),
        pk: "id".to_owned(),
        algorithm: ShardAlgorithm::HashMod {
            shards: SHARDS.len() as u32,
        },
        shards: SHARDS
            .iter()
            .map(|name| ShardEndpoint {
                name: (*name).to_owned(),
                url: None,
            })
            .collect(),
    }
}

/// 每个分片一个独立客户端，`behaviors` 与 [`SHARDS`] 同序。
fn router(behaviors: [Behavior; SHARDS.len()]) -> (ShardRouter, Calls) {
    let calls: Calls = Arc::default();
    let clients: HashMap<String, Arc<dyn RdbmsClient>> = SHARDS
        .into_iter()
        .zip(behaviors)
        .map(|(name, behavior)| {
            let client: Arc<dyn RdbmsClient> = Arc::new(FakeClient {
                behavior,
                calls: Arc::clone(&calls),
            });
            (name.to_owned(), client)
        })
        .collect();
    let catalog = ShardCatalog {
        tables: vec![table()],
    };
    (ShardRouter::new(catalog, clients).unwrap(), calls)
}

fn request() -> ScatterRequest {
    ScatterRequest {
        table: "users".to_owned(),
        sql: SQL.to_owned(),
        params: vec![json!(7)],
        order_by: "id".to_owned(),
        limit: 10,
        offset: 0,
        timeout: Duration::from_secs(5),
        max_concurrency: SHARDS.len(),
    }
}

fn row(id: i64, shard: &str) -> Row {
    Row::new(
        vec!["id".to_owned(), "shard".to_owned()],
        vec![json!(id), json!(shard)],
    )
}

/// 自定义主键与列值的行，用于 NULL / 类型混排 / 缺列分支。
fn row_with(id: Value, column: &str, value: Value) -> Row {
    Row::new(vec!["id".to_owned(), column.to_owned()], vec![id, value])
}

fn empty_shards() -> [Behavior; SHARDS.len()] {
    [(); SHARDS.len()].map(|_| Behavior::Rows(Vec::new()))
}

fn ids(rows: &[Row]) -> Vec<i64> {
    rows.iter()
        .map(|row| row.get("id").and_then(Value::as_i64).unwrap())
        .collect()
}

#[tokio::test]
async fn scatter_merges_and_dedupes_by_pk() {
    let (router, calls) = router([
        Behavior::Rows(vec![row(1, "s0"), row(2, "s0")]),
        Behavior::Rows(vec![row(2, "s1"), row(3, "s1")]),
        Behavior::Rows(vec![row(3, "s2"), row(4, "s2")]),
        Behavior::Rows(Vec::new()),
    ]);

    let req = request();
    let rows = router.scatter(&req).await.unwrap();

    assert_eq!(ids(&rows), vec![1, 2, 3, 4]);
    // 重复主键保留分片下标最小的那份（排序的次键），与并发完成顺序无关。
    assert_eq!(rows[1].get("shard"), Some(&json!("s0")));
    assert_eq!(rows[2].get("shard"), Some(&json!("s1")));
    // SQL 与参数原样发到每个分片，各一次。
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), SHARDS.len());
    for (sql, params) in calls.iter() {
        assert_eq!(sql, SQL);
        assert_eq!(params.as_slice(), req.params.as_slice());
    }
}

#[tokio::test]
async fn scatter_sorts_before_paginating() {
    let (router, _) = router([
        Behavior::Rows(vec![row(5, "s0"), row(1, "s0")]),
        Behavior::Rows(vec![row(3, "s1"), row(3, "s1")]),
        Behavior::Rows(vec![row(2, "s2"), row(4, "s2")]),
        Behavior::Rows(Vec::new()),
    ]);

    let mut req = request();
    req.offset = 1;
    req.limit = 3;
    let rows = router.scatter(&req).await.unwrap();

    // 排序 → 去重（3 出现两次）→ 分页：[1,2,3,4,5] 的 1..4。
    assert_eq!(ids(&rows), vec![2, 3, 4]);
}

#[tokio::test]
async fn scatter_partial_failure_returns_error() {
    let (router, _) = router([
        Behavior::Rows(vec![row(1, "s0")]),
        Behavior::Fail,
        Behavior::Rows(vec![row(2, "s2")]),
        Behavior::Rows(vec![row(3, "s3")]),
    ]);

    let err = router.scatter(&request()).await.unwrap_err();

    assert_eq!(
        err,
        ShardError::PartialFailure {
            failed: vec!["s1".to_owned()]
        }
    );
}

#[tokio::test]
async fn scatter_unknown_table_is_error() {
    let (router, calls) = router(empty_shards());
    let mut req = request();
    req.table = "missing".to_owned();

    assert_eq!(
        router.scatter(&req).await.unwrap_err(),
        ShardError::UnknownTable("missing".to_owned())
    );
    assert!(calls.lock().unwrap().is_empty(), "未知表不得触达任何分片");
}

#[tokio::test]
async fn scatter_timeout_returns_error() {
    let (router, _) = router([(); SHARDS.len()].map(|_| Behavior::Sleep(Duration::from_secs(60))));
    let mut req = request();
    req.timeout = Duration::from_millis(10);

    let err = router.scatter(&req).await.unwrap_err();

    // 60s 的 sleep 全在超时之后；没有一个分片完成，失败名单就是全部分片。
    let all: Vec<String> = SHARDS.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(err, ShardError::PartialFailure { failed: all });
}

#[tokio::test]
async fn scatter_concurrency_is_bounded() {
    let gate = Arc::new(Gate {
        in_flight: AtomicUsize::new(0),
        seen_max: AtomicUsize::new(0),
        permits: Semaphore::new(0),
    });
    let (router, _) = router([
        Behavior::Gated {
            gate: Arc::clone(&gate),
            rows: vec![row(1, "s0")],
        },
        Behavior::Gated {
            gate: Arc::clone(&gate),
            rows: vec![row(2, "s1")],
        },
        Behavior::Gated {
            gate: Arc::clone(&gate),
            rows: vec![row(3, "s2")],
        },
        Behavior::Gated {
            gate: Arc::clone(&gate),
            rows: vec![row(4, "s3")],
        },
    ]);
    let mut req = request();
    req.max_concurrency = 2;
    let handle = tokio::spawn(async move { router.scatter(&req).await });

    // 等调度器静止：连续多轮 yield 后计数不变，说明在飞任务都卡在 gate 上。
    let mut last = usize::MAX;
    let mut stable = 0;
    for _ in 0..10_000 {
        tokio::task::yield_now().await;
        let now = gate.in_flight.load(Ordering::SeqCst);
        stable = if now == last { stable + 1 } else { 0 };
        last = now;
        if stable >= 100 {
            break;
        }
    }
    // 恰好等于上限：少于 2 说明没有真正并发，多于 2 说明上限失守。
    assert_eq!(
        gate.seen_max.load(Ordering::SeqCst),
        2,
        "同时在飞的分片数应恰好等于 max_concurrency"
    );
    assert_eq!(gate.in_flight.load(Ordering::SeqCst), 2);

    gate.permits.add_permits(2 * SHARDS.len());
    let rows = handle.await.unwrap().unwrap();

    assert_eq!(ids(&rows), vec![1, 2, 3, 4]);
}

/// 断言是 `Config` 且消息点名了出问题的列。
fn assert_config_mentions(err: ShardError, needle: &str) {
    match err {
        ShardError::Config(message) => {
            assert!(
                message.contains(needle),
                "message `{message}` 缺少 `{needle}`"
            )
        }
        other => panic!("expected Config, got {other:?}"),
    }
}

#[tokio::test]
async fn scatter_null_order_by_is_error() {
    let (router, _) = router([
        Behavior::Rows(vec![row_with(json!(1), "rank", Value::Null)]),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
    ]);
    let mut req = request();
    req.order_by = "rank".to_owned();

    // 跨库 NULL 排序语义不一致，放行会让归并顺序随分片实现漂移。
    assert_config_mentions(router.scatter(&req).await.unwrap_err(), "order_by");
}

#[tokio::test]
async fn scatter_missing_order_by_is_error() {
    let (router, _) = router([
        Behavior::Rows(vec![Row::new(vec!["id".to_owned()], vec![json!(1)])]),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
    ]);
    let mut req = request();
    req.order_by = "rank".to_owned();

    assert_config_mentions(router.scatter(&req).await.unwrap_err(), "order_by");
}

#[tokio::test]
async fn scatter_null_pk_is_error() {
    let (router, _) = router([
        Behavior::Rows(vec![row_with(Value::Null, "rank", json!(1))]),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
    ]);
    let mut req = request();
    req.order_by = "rank".to_owned();

    assert_config_mentions(router.scatter(&req).await.unwrap_err(), "pk");
}

#[tokio::test]
async fn scatter_missing_pk_is_error() {
    let (router, _) = router([
        Behavior::Rows(vec![Row::new(vec!["rank".to_owned()], vec![json!(1)])]),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
        Behavior::Rows(Vec::new()),
    ]);
    let mut req = request();
    req.order_by = "rank".to_owned();

    assert_config_mentions(router.scatter(&req).await.unwrap_err(), "pk");
}

#[tokio::test]
async fn scatter_zero_concurrency_is_error() {
    let (router, calls) = router(empty_shards());
    let mut req = request();
    req.max_concurrency = 0;

    // 0 个许可会永远挂起，必须在信任边界拒绝；且不得触达任何分片。
    assert_config_mentions(router.scatter(&req).await.unwrap_err(), "max_concurrency");
    assert!(calls.lock().unwrap().is_empty());
}

/// NULL 已在 `merge` 入口被拒，走不到 `compare_values`；其全序位置只能直接测。
#[test]
fn scatter_compare_values_is_total_across_types() {
    let ordered = [
        Value::Null,
        json!(false),
        json!(-1),
        json!(2),
        json!(2.5),
        json!(10),
        json!("a"),
        json!("b"),
    ];
    for pair in ordered.windows(2) {
        assert_eq!(
            super::compare_values(&pair[0], &pair[1]),
            std::cmp::Ordering::Less,
            "{:?} 应排在 {:?} 之前",
            pair[0],
            pair[1]
        );
        assert_eq!(
            super::compare_values(&pair[1], &pair[0]),
            std::cmp::Ordering::Greater
        );
    }
}

#[tokio::test]
async fn scatter_orders_mixed_json_types_deterministically() {
    let (router, _) = router([
        Behavior::Rows(vec![
            row_with(json!(2), "k", json!(10)),
            row_with(json!(3), "k", json!("b")),
        ]),
        Behavior::Rows(vec![
            row_with(json!(4), "k", json!(2)),
            row_with(json!(5), "k", json!("a")),
        ]),
        Behavior::Rows(vec![
            row_with(json!(6), "k", json!(2.5)),
            row_with(json!(7), "k", json!(false)),
        ]),
        Behavior::Rows(vec![row_with(json!(8), "k", json!(3.0))]),
    ]);
    let mut req = request();
    req.order_by = "k".to_owned();

    let rows = router.scatter(&req).await.unwrap();

    // 全序：bool < 数字（按数值，int/float 混排）< 字符串。
    assert_eq!(ids(&rows), vec![7, 4, 6, 8, 2, 5, 3]);
}

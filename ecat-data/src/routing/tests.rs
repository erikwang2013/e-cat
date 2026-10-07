// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 行为矩阵测试：写落主、读落从轮询、跳过已熔断的副本、全熔断时的两种降级行为。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::RdbmsRouting;
use crate::dialect::Dialect;
use crate::rdbms::{RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};

/// 共享调用日志：每次**真正落到某个端点**的调用记一笔 `"名字:方法"`。
/// `dialect()` 是本地判断、不产生 I/O，故不记。
#[derive(Clone)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn push(&self, name: &str, method: &str) {
        self.0.lock().unwrap().push(format!("{name}:{method}"));
    }

    /// `prefix` 形如 `"r1:"` 的调用次数。
    fn hits(&self, prefix: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.starts_with(prefix))
            .count()
    }

    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// 假端点。`fail` 的端点 SQL 一律报错（模拟挂掉的从库），
/// 但 `transaction()` 不失败 —— 那是「事务不经熔断」用例的前提。
struct FakeClient {
    name: &'static str,
    dialect: Dialect,
    log: Log,
    fail: bool,
}

impl FakeClient {
    fn shared(name: &'static str, dialect: Dialect, log: &Log, fail: bool) -> Arc<dyn RdbmsClient> {
        Arc::new(Self {
            name,
            dialect,
            log: log.clone(),
            fail,
        })
    }

    fn record_sql<T>(&self, method: &str, ok: T) -> Result<T, RdbmsError> {
        self.log.push(self.name, method);
        if self.fail {
            Err(RdbmsError::Connection("replica down".into()))
        } else {
            Ok(ok)
        }
    }
}

#[async_trait]
impl SqlExecutor for FakeClient {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        self.record_sql("execute", 1)
    }
    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.record_sql("query", vec![])
    }
    async fn execute_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.record_sql("execute_with", 1)
    }
    async fn query_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.record_sql("query_with", vec![])
    }
    async fn query_write(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.record_sql("query_write", vec![])
    }
    async fn execute_then_query(
        &self,
        _first: &str,
        _first_params: &[serde_json::Value],
        _second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.record_sql("execute_then_query", vec![])
    }
    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

#[async_trait]
impl RdbmsClient for FakeClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.log.push(self.name, "transaction");
        Ok(Transaction::new())
    }
}

/// 造路由：`(名字, 方言, 是否失败)`，**第一个是主库**，其余是副本。
fn build(spec: &[(&'static str, Dialect, bool)], log: &Log) -> RdbmsRouting {
    let mut clients: Vec<Arc<dyn RdbmsClient>> = spec
        .iter()
        .map(|(name, dialect, fail)| FakeClient::shared(name, *dialect, log, *fail))
        .collect();
    // `remove` 而非 `remove(0)` 之外的写法：保证主库从列表里摘掉，不重复计。
    let primary = clients.remove(0);
    RdbmsRouting::new(primary, clients)
}

const PG: Dialect = Dialect::Postgres;
const LITE: Dialect = Dialect::Sqlite;

/// 打满某个副本的熔断窗口：**失败率 1.0、窗口满 5 次** → `Open`。
/// 轮询会把读分散到各副本，所以按「这个副本吃满 5 次」为准，不看总读次数。
async fn drive_until_open(r: &RdbmsRouting, log: &Log, name: &str) {
    for _ in 0..64 {
        let _ = r.query("SELECT 1").await;
        if log.hits(&format!("{name}:")) >= 5 {
            return;
        }
    }
    panic!("{name} 没能在 64 次读内累积 5 次失败");
}

#[tokio::test]
async fn writes_land_on_primary() {
    let log = Log::new();
    let r = build(&[("primary", PG, false), ("r1", LITE, false)], &log);

    r.execute("DELETE FROM t").await.unwrap();
    r.execute_with("UPDATE t SET v = ?", &[]).await.unwrap();

    assert_eq!(
        log.entries(),
        vec!["primary:execute", "primary:execute_with"]
    );
}

#[tokio::test]
async fn reads_land_on_replicas() {
    let log = Log::new();
    let r = build(&[("primary", PG, false), ("r1", LITE, false)], &log);

    r.query("SELECT 1").await.unwrap();
    r.query_with("SELECT ?", &[]).await.unwrap();

    assert_eq!(log.entries(), vec!["r1:query", "r1:query_with"]);
    assert_eq!(log.hits("primary:"), 0);
}

/// `query_write` 是**写路径**的返回行查询（`INSERT ... RETURNING`）：
/// 落从库会读到陈旧数据，必须落主库。
#[tokio::test]
async fn query_write_lands_on_primary() {
    let log = Log::new();
    let r = build(&[("primary", PG, false), ("r1", LITE, false)], &log);

    r.query_write("INSERT INTO t (v) VALUES (?) RETURNING id", &[])
        .await
        .unwrap();

    assert_eq!(log.entries(), vec!["primary:query_write"]);
}

/// 两步式插入（MySQL 的 `LAST_INSERT_ID()`）也是写路径 → 必须落主库。
#[tokio::test]
async fn execute_then_query_lands_on_primary() {
    let log = Log::new();
    let r = build(&[("primary", PG, false), ("r1", LITE, false)], &log);

    r.execute_then_query(
        "INSERT INTO t (v) VALUES (?)",
        &[],
        "SELECT LAST_INSERT_ID()",
    )
    .await
    .unwrap();

    assert_eq!(log.entries(), vec!["primary:execute_then_query"]);
}

#[tokio::test]
async fn round_robin_cycles_replicas_in_order() {
    let log = Log::new();
    let r = build(
        &[
            ("primary", PG, false),
            ("r1", LITE, false),
            ("r2", LITE, false),
            ("r3", LITE, false),
        ],
        &log,
    );

    for _ in 0..4 {
        r.query("SELECT 1").await.unwrap();
    }

    assert_eq!(
        log.entries(),
        vec!["r1:query", "r2:query", "r3:query", "r1:query"],
        "轮询顺序必须可预测"
    );
}

/// spec:235-236 的落点：副本熔断后**不再被选中**。
/// 否则轮询仍把 1/N 的读转过去靠熔断快速失败 —— 那不是故障隔离，
/// 是稳定的 1/N 失败率。
#[tokio::test]
async fn open_replica_is_skipped() {
    let log = Log::new();
    let r = build(
        &[
            ("primary", PG, false),
            ("r1", LITE, true),
            ("r2", LITE, false),
        ],
        &log,
    )
    .fallback_to_primary(false);

    drive_until_open(&r, &log, "r1").await;
    assert_eq!(log.hits("r1:"), 5, "r1 应当恰好吃满 5 次失败");

    let r1_hits = log.hits("r1:");
    let r2_hits = log.hits("r2:");
    for _ in 0..6 {
        // r1 已熔断：读只能落 r2，且必须成功 —— 若仍轮询到 r1，
        // 这里要么 Err（快速失败）要么 r2 计数少 6 次。
        r.query("SELECT 1")
            .await
            .expect("r1 熔断后读必须成功落到 r2");
    }
    assert_eq!(log.hits("r1:"), r1_hits, "熔断打开的 r1 不得再被选中");
    assert_eq!(log.hits("r2:"), r2_hits + 6);
    assert_eq!(log.hits("primary:"), 0, "还有可用副本时不该读主");
}

/// 副本全熔断 + `fallback_to_primary = true`（默认）→ 读降级到主库。
#[tokio::test]
async fn all_open_replicas_fall_back_to_primary() {
    let log = Log::new();
    let r = build(
        &[
            ("primary", PG, false),
            ("r1", LITE, true),
            ("r2", LITE, true),
        ],
        &log,
    );

    drive_until_open(&r, &log, "r1").await;
    drive_until_open(&r, &log, "r2").await;

    let primary_hits = log.hits("primary:");
    r.query("SELECT 1").await.expect("副本全熔断时应降级读主");
    r.query_with("SELECT ?", &[])
        .await
        .expect("query_with 同样降级");
    assert_eq!(log.hits("primary:"), primary_hits + 2);
}

/// 副本全熔断 + `fallback_to_primary = false` → 快速失败，且**不偷偷读主**。
#[tokio::test]
async fn all_open_replicas_without_fallback_report_no_available_replica() {
    let log = Log::new();
    let r = build(
        &[
            ("primary", PG, false),
            ("r1", LITE, true),
            ("r2", LITE, true),
        ],
        &log,
    )
    .fallback_to_primary(false);

    drive_until_open(&r, &log, "r1").await;
    drive_until_open(&r, &log, "r2").await;

    let err = r.query("SELECT 1").await.unwrap_err();
    assert!(
        matches!(err, RdbmsError::NoAvailableReplica),
        "got: {err:?}"
    );
    let err = r.query_with("SELECT ?", &[]).await.unwrap_err();
    assert!(
        matches!(err, RdbmsError::NoAvailableReplica),
        "got: {err:?}"
    );
    assert_eq!(log.hits("primary:"), 0, "关闭降级后不得读主");
}

/// 没配副本时：降级开 → 读主；关闭降级 → `NoAvailableReplica`。
#[tokio::test]
async fn no_replicas_behaves_like_all_unavailable() {
    let log = Log::new();
    let r = build(&[("primary", PG, false)], &log);
    r.query("SELECT 1").await.unwrap();
    assert_eq!(log.entries(), vec!["primary:query"]);

    let log = Log::new();
    let r = build(&[("primary", PG, false)], &log).fallback_to_primary(false);
    let err = r.query("SELECT 1").await.unwrap_err();
    assert!(
        matches!(err, RdbmsError::NoAvailableReplica),
        "got: {err:?}"
    );
}

/// 事务永远走主库，且**不经熔断** —— 主库熔断器打开后事务仍打得通。
#[tokio::test]
async fn transaction_lands_on_primary_without_breaker() {
    let log = Log::new();
    let r = build(&[("primary", PG, true), ("r1", LITE, false)], &log);

    for _ in 0..5 {
        assert!(r.execute("UPDATE t SET v = 1").await.is_err());
    }
    // 证明主库熔断器确已 `Open`：这一笔没落到客户端（快速失败）。
    let primary_hits = log.hits("primary:execute");
    assert!(r.execute("UPDATE t SET v = 1").await.is_err());
    assert_eq!(log.hits("primary:execute"), primary_hits);

    r.transaction()
        .await
        .expect("事务不经熔断：主库熔断器打开也必须能开事务");
    assert_eq!(log.hits("primary:transaction"), 1);
    assert_eq!(log.hits("r1:transaction"), 0, "事务不得走副本");
    let _ = r.fallback_to_primary(true);
}

#[tokio::test]
async fn dialect_comes_from_primary() {
    let log = Log::new();
    let r = build(&[("primary", PG, false), ("r1", LITE, false)], &log);

    assert_eq!(r.dialect(), PG);
    assert!(
        log.entries().is_empty(),
        "dialect() 是本地判断，不该产生调用"
    );
}

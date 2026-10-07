// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! feature = "metrics"：把连接池与超时计数接进 `ecat_metrics::registry()` ——
//! 已有的 `/metrics` 端点自动多出这四个指标（spec:722）：
//!
//! | 指标 | 类型 | 维度 |
//! |---|---|---|
//! | `ecat_rdbms_pool_connections` | gauge | `backend`、`state="idle"\|"active"` |
//! | `ecat_rdbms_pool_timeouts_total` | counter | `backend` |
//! | `ecat_rdbms_query_timeout_total` | counter | `backend` |
//! | `ecat_rdbms_transactions_leaked_total` | counter | `backend` |
//!
//! 与 `ecat-data-sqlx` 的同名模块同构：池连接数是**抓取时现读**的（手写
//! [`Collector`]），后两个 counter 读 `ecat-data` 的进程级静态量
//! （[`QUERY_TIMEOUTS`] / [`TRANSACTIONS_LEAKED`]）—— 把日志变成可告警的指标，
//! 不另建计数器（spec:726-727）。
//!
//! 两处驱动差异：池换成了 deadpool，连接数从 [`Status`] 读（sqlx 那边是
//! `size()` / `idle()` 两个方法）；等连接超时走
//! [`crate::client::pool_err`]，不是 sqlx 的 `db_err`。

use crate::pool::MssqlManager;
use deadpool::managed::Pool;
use ecat_data::{QUERY_TIMEOUTS, TRANSACTIONS_LEAKED};
use prometheus::core::{Collector, Desc};
use prometheus::proto::{Counter, Gauge, LabelPair, Metric, MetricFamily, MetricType};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// 取连接的累计超时次数（[`count_pool_timeout`] 递增）。
///
/// 不复用 [`QUERY_TIMEOUTS`]：那个数的是**查询**超时（连接已经拿到手），这里数的是
/// **等连接**超时 —— 前者查慢查询、后者查池容量，混成一个指标会同时丢掉两条线索。
static POOL_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// 记一次「等连接超时」。由 [`crate::client::pool_err`] 在见到
/// `PoolError::Timeout` 时调用。
pub(crate) fn count_pool_timeout() {
    POOL_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

/// 注册池指标。`backend` 是标签值（`"primary"` / `"replica-1"` 之类）。
///
/// 幂等：collector 只注册一次，重复调用只是多挂一个被观测的池；同一个 `backend`
/// 重复注册则覆盖（避免同一标签出现两份样本）。
///
/// 唯一会失败的情形是 registry 里已有同名指标 —— 那时四个指标都不会输出。
pub fn register_pool_metrics(backend: &'static str, pool: &Pool<MssqlManager>) {
    let pools = POOLS.get_or_init(|| {
        let pools = Arc::new(Pools::new());
        // 与 ecat-metrics 自己的注册同款：AlreadyReg 只可能是名字撞车，
        // 而这里的名字是本 crate 独占的。
        let _ = ecat_metrics::registry().register(Box::new(Registered(Arc::clone(&pools))));
        pools
    });
    pools.add(backend, pool.clone());
}

/// 真正注册进 registry 的那层壳。
///
/// 不能直接 `impl Collector for Arc<Pools>`：`Arc` 是外部类型，孤儿规则不允许
/// （E0117）。包一层自有类型即可，内部仍是同一个 [`Pools`]。
struct Registered(Arc<Pools>);

impl Collector for Registered {
    fn desc(&self) -> Vec<&Desc> {
        self.0.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        self.0.collect()
    }
}

/// deadpool 的 [`Status`] → `(idle, active)`。
///
/// deadpool 的 `available` 是**池内空闲**；`size` 是已建立的连接总数（含在用的），
/// 所以在用 = `size - available`。`saturating_sub` 只是防御：两者理论同源，
/// 但被别处改坏时宁可报 0 也不要 panic 在抓取路径上。
fn idle_active(status: &deadpool::Status) -> (u64, u64) {
    let available = status.available as u64;
    (available, (status.size as u64).saturating_sub(available))
}

/// 全局 collector：一个进程一个，四个指标家族都在它身上。
///
/// 为什么不是「一个池一个 collector」：`Registry` 按「指标名 + 常量标签」去重，
/// 同名 collector 注册第二次直接 `AlreadyReg` —— 多后端共用一个 registry 时
/// 只有合并成单 collector 这一条路。
static POOLS: OnceLock<Arc<Pools>> = OnceLock::new();

struct Pools {
    /// `backend` 标签 → 池。持有 `Pool` 本身（deadpool 的 `Pool` 内部是 `Arc`，
    /// 克隆很轻）。
    pools: Mutex<Vec<(&'static str, Pool<MssqlManager>)>>,
    descs: [Desc; 4],
}

impl Pools {
    fn new() -> Self {
        Self {
            pools: Mutex::new(Vec::new()),
            descs: [
                desc(
                    "ecat_rdbms_pool_connections",
                    "Connections in the RDBMS connection pool",
                    &["backend", "state"],
                ),
                desc(
                    "ecat_rdbms_pool_timeouts_total",
                    "Total RDBMS pool acquire timeouts",
                    &["backend"],
                ),
                desc(
                    "ecat_rdbms_query_timeout_total",
                    "Total RDBMS query timeouts",
                    &["backend"],
                ),
                desc(
                    "ecat_rdbms_transactions_leaked_total",
                    "Total RDBMS transactions dropped without commit or rollback",
                    &["backend"],
                ),
            ],
        }
    }

    fn add(&self, backend: &'static str, pool: Pool<MssqlManager>) {
        let mut pools = self.pools.lock().unwrap();
        match pools.iter_mut().find(|(name, _)| *name == backend) {
            Some(slot) => slot.1 = pool,
            None => pools.push((backend, pool)),
        }
    }
}

impl Collector for Pools {
    fn desc(&self) -> Vec<&Desc> {
        self.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let pools = self.pools.lock().unwrap();
        let mut connections = Vec::with_capacity(pools.len() * 2);
        let mut pool_timeouts = Vec::with_capacity(pools.len());
        let mut query_timeouts = Vec::with_capacity(pools.len());
        let mut leaked = Vec::with_capacity(pools.len());

        // 四个 counter/静态量对每个 backend 各出一份样本：`QUERY_TIMEOUTS` 等是
        // 进程级的，按 backend 重复计数是既定代价（spec:726 要求的接法）。
        let timeouts = count(QUERY_TIMEOUTS.load(Ordering::Relaxed));
        let leaks = count(TRANSACTIONS_LEAKED.load(Ordering::Relaxed));
        let acquire_timeouts = count(POOL_TIMEOUTS.load(Ordering::Relaxed));

        for (backend, pool) in pools.iter() {
            let backend = *backend;
            let (idle, active) = idle_active(&pool.status());
            // `as f64` 而不是 `f64::from`：`From<u64> for f64` 不存在（只有 u8/u16/u32）。
            connections.push(gauge(
                labels(&[("backend", backend), ("state", "idle")]),
                idle as f64,
            ));
            connections.push(gauge(
                labels(&[("backend", backend), ("state", "active")]),
                active as f64,
            ));
            pool_timeouts.push(counter(labels(&[("backend", backend)]), acquire_timeouts));
            query_timeouts.push(counter(labels(&[("backend", backend)]), timeouts));
            leaked.push(counter(labels(&[("backend", backend)]), leaks));
        }

        vec![
            family(
                "ecat_rdbms_pool_connections",
                "Connections in the RDBMS connection pool",
                MetricType::GAUGE,
                connections,
            ),
            family(
                "ecat_rdbms_pool_timeouts_total",
                "Total RDBMS pool acquire timeouts",
                MetricType::COUNTER,
                pool_timeouts,
            ),
            family(
                "ecat_rdbms_query_timeout_total",
                "Total RDBMS query timeouts",
                MetricType::COUNTER,
                query_timeouts,
            ),
            family(
                "ecat_rdbms_transactions_leaked_total",
                "Total RDBMS transactions dropped without commit or rollback",
                MetricType::COUNTER,
                leaked,
            ),
        ]
    }
}

fn count(v: u64) -> f64 {
    v as f64
}

fn desc(name: &str, help: &str, labels: &[&str]) -> Desc {
    Desc::new(
        name.to_string(),
        help.to_string(),
        labels.iter().map(|l| (*l).to_string()).collect(),
        HashMap::new(),
    )
    .expect("指标名与标签名合法")
}

fn labels(pairs: &[(&str, &str)]) -> Vec<LabelPair> {
    pairs
        .iter()
        .map(|(name, value)| {
            let mut l = LabelPair::default();
            l.set_name((*name).to_string());
            l.set_value((*value).to_string());
            l
        })
        .collect()
}

fn gauge(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut m = Metric::default();
    m.set_label(labels.into());
    let mut g = Gauge::default();
    g.set_value(value);
    m.set_gauge(g);
    m
}

fn counter(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut m = Metric::default();
    m.set_label(labels.into());
    let mut c = Counter::default();
    c.set_value(value);
    m.set_counter(c);
    m
}

fn family(name: &str, help: &str, kind: MetricType, metrics: Vec<Metric>) -> MetricFamily {
    let mut f = MetricFamily::default();
    f.set_name(name.to_string());
    f.set_help(help.to_string());
    f.set_field_type(kind);
    f.set_metric(metrics.into());
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MssqlClient;
    use deadpool::managed::{PoolError, TimeoutType};
    use ecat_data::RdbmsError;

    /// 本环境没有 SQL Server，但建池不需要连库（deadpool 惰性建连）——
    /// 登记、抓取、超时计数都照常能验。
    async fn client() -> MssqlClient {
        MssqlClient::connect("mssql://sa:pw@127.0.0.1:1433/db")
            .await
            .unwrap()
    }

    /// 抓取后按 `指标名{backend="..."}` 找样本值。找不到就是 None —— 断言
    /// 「指标压根没出现」与「值不对」是两码事，测试要能分开报。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// `state` 的映射：deadpool 的 `available` 是空闲，`size - available` 是在用。
    /// 手搓 [`deadpool::Status`] 是唯一能在没有真库时验证这条映射的办法。
    #[test]
    fn idle_active_maps_deadpool_status() {
        let status = deadpool::Status {
            max_size: 10,
            size: 3,
            available: 1,
            waiting: 0,
        };
        assert_eq!(idle_active(&status), (1, 2), "在用 = size - available");

        let empty = deadpool::Status {
            max_size: 10,
            size: 0,
            available: 0,
            waiting: 0,
        };
        assert_eq!(idle_active(&empty), (0, 0));
    }

    /// 样本值必须落在 `[lo, hi]` 内 —— 理由见 [`registers_all_four_metrics_for_the_backend`]。
    fn assert_bracketed(text: &str, prefix: &str, lo: u64, hi: u64) {
        let v =
            sample(text, prefix).unwrap_or_else(|| panic!("缺指标 {prefix}，实际输出:\n{text}"));
        assert!(
            v >= lo as f64 && v <= hi as f64,
            "{prefix} 应落在 [{lo}, {hi}]，实际 {v} —— 必须直读进程级静态量"
        );
    }

    /// 四个指标都要出现，且 counter 的值**必须落在** `ecat-data` 静态量的区间里 ——
    /// 这正是「不另建计数器」的验收点。
    ///
    /// 为什么是区间而不是等值：同一测试进程里别的用例（查询超时、事务泄漏）
    /// 也会推进这些静态量，cargo 默认并行跑用例，精确值会被顶掉。区间上界在
    /// **抓取之后**才读，所以并发推进只会把区间撑大、不会误报；下界由本用例
    /// 自己推一格（`fetch_add` 返回旧值，`+1` 即新值），保证区间非退化 ——
    /// 另建一个计数器的实现凑不出这个数。
    #[tokio::test]
    async fn registers_all_four_metrics_for_the_backend() {
        let c = client().await;
        register_pool_metrics("mssql-test", c.pool());

        let q_lo = QUERY_TIMEOUTS.fetch_add(1, Ordering::Relaxed) + 1;
        let l_lo = TRANSACTIONS_LEAKED.fetch_add(1, Ordering::Relaxed) + 1;

        let text = ecat_metrics::metrics_text();

        let q_hi = QUERY_TIMEOUTS.load(Ordering::Relaxed);
        let l_hi = TRANSACTIONS_LEAKED.load(Ordering::Relaxed);

        for name in [
            "ecat_rdbms_pool_connections",
            "ecat_rdbms_pool_timeouts_total",
            "ecat_rdbms_query_timeout_total",
            "ecat_rdbms_transactions_leaked_total",
        ] {
            assert!(
                text.contains(&format!("{name}{{backend=\"mssql-test\"")),
                "缺指标 {name}，实际输出:\n{text}"
            );
        }
        assert_bracketed(
            &text,
            "ecat_rdbms_query_timeout_total{backend=\"mssql-test\"}",
            q_lo,
            q_hi,
        );
        assert_bracketed(
            &text,
            "ecat_rdbms_transactions_leaked_total{backend=\"mssql-test\"}",
            l_lo,
            l_hi,
        );

        // 池是空的：两个 state 都必须是 0（gauge 是抓取时现读，不是注册时快照）。
        assert_eq!(
            sample(
                &text,
                "ecat_rdbms_pool_connections{backend=\"mssql-test\",state=\"idle\"}"
            ),
            Some(0.0)
        );
        assert_eq!(
            sample(
                &text,
                "ecat_rdbms_pool_connections{backend=\"mssql-test\",state=\"active\"}"
            ),
            Some(0.0)
        );
    }

    /// 等连接超时走 `pool_err` 这条唯一漏斗（`MssqlClient` 的取连接全在这里收口）：
    /// 它必须把 `PoolError::Timeout` 计数，别的变体不计，而且这个数要能抓出来。
    ///
    /// 这里的值可以钉精确：本测试进程里只有这条用例会推进 `POOL_TIMEOUTS`
    /// （`tracing` 的端到端用例走的是查询超时，不经过 `pool_err`）。
    #[tokio::test]
    async fn pool_timeout_is_counted_by_pool_err() {
        let before = POOL_TIMEOUTS.load(Ordering::Relaxed);
        let err = crate::client::pool_err(PoolError::Timeout(TimeoutType::Wait));
        assert!(matches!(err, RdbmsError::Connection(_)), "got: {err:?}");
        let counted = POOL_TIMEOUTS.load(Ordering::Relaxed);
        assert_eq!(counted, before + 1);

        // 别的变体（真实建连失败就是这一支）不该被算进来。
        let err = crate::client::pool_err(PoolError::Backend(RdbmsError::Database("x".into())));
        assert!(matches!(err, RdbmsError::Database(_)));
        assert_eq!(
            POOL_TIMEOUTS.load(Ordering::Relaxed),
            counted,
            "非 Timeout 不该计数"
        );

        // 记的这一笔必须出现在指标里（区间上界放在抓取之后）。
        let c = client().await;
        register_pool_metrics("mssql-timeout-test", c.pool());
        let text = ecat_metrics::metrics_text();
        let hi = POOL_TIMEOUTS.load(Ordering::Relaxed);
        assert_bracketed(
            &text,
            "ecat_rdbms_pool_timeouts_total{backend=\"mssql-timeout-test\"}",
            counted,
            hi,
        );
    }

    /// 同一个 backend 注册两次只应留一份样本（否则 Prometheus 会因重复样本报错）。
    #[tokio::test]
    async fn same_backend_registration_replaces_instead_of_duplicating() {
        let a = client().await;
        let b = client().await;
        register_pool_metrics("mssql-dup-test", a.pool());
        register_pool_metrics("mssql-dup-test", b.pool());

        let text = ecat_metrics::metrics_text();
        let hits = text
            .lines()
            .filter(|l| {
                l.starts_with(
                    "ecat_rdbms_pool_connections{backend=\"mssql-dup-test\",state=\"idle\"}",
                )
            })
            .count();
        assert_eq!(hits, 1, "重复注册应覆盖而不是追加，实际输出:\n{text}");
    }
}

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
//! 池连接数是**抓取时现读**的（取数闭包挂在 `ecat_metrics` 的唯一 collector 上）：
//! 本 crate 的查询路径里没有「取连接」的钩子（取用由 sqlx 内部完成），没有哪个
//! 调用点能顺手 `set` 一下。后两个 counter 读的是 `ecat-data` 的进程级静态量
//! （[`timeout_counter`] 取的 `TIMEOUTS` Rdbms 维度 / [`TRANSACTIONS_LEAKED`]）—— 把日志变成可告警的指标，
//! 不另建计数器（spec:726-727）。

use crate::pool::Pool;
use ecat_data::{BackendKind, TRANSACTIONS_LEAKED, timeout_counter};
use std::sync::atomic::{AtomicU64, Ordering};

/// 取连接的累计超时次数（[`count_pool_timeout`] 递增）。
///
/// 不复用 [`timeout_counter`]`(BackendKind::Rdbms)` 那个槽：那个数的是**查询**超时（连接已经拿到手），这里数的是
/// **等连接**超时 —— 前者查慢查询、后者查池容量，混成一个指标会同时丢掉两条线索。
static POOL_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// 记一次「等连接超时」。由 `db_err` 在见到 `sqlx::Error::PoolTimedOut` 时调用。
pub(crate) fn count_pool_timeout() {
    POOL_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

/// 注册池指标。`backend` 是标签值（`"primary"` / `"replica-1"` 之类）。
///
/// 幂等：重复调用只是多挂一个被观测的池；同一个 `backend` 重复注册则覆盖
/// （避免同一标签出现两份样本）。
///
/// **四个家族的全进程唯一 collector 在 `ecat_metrics`**（`ecat-metrics/src/rdbms.rs`）：
/// 本函数只把本 crate 的取数闭包挂上去。早期版本在这里自建 collector —— 那会与
/// `ecat-data-mssql` 的同名家族互相顶掉（`Registry` 只留先注册的那一份，
/// 后者的四个指标静默消失）。
pub fn register_pool_metrics(backend: &'static str, pool: &Pool) {
    // `Pool` 内部是 `Arc`，克隆只是多一个句柄；闭包要 `'static`，所以按值捕获。
    let pool = pool.clone();
    ecat_metrics::register_rdbms_metrics(
        backend,
        // 一次读取同时给出两个投影：分两次读会在并发下出现 idle + active > size。
        Box::new(move || {
            let idle = u64::from(pool.idle());
            let size = u64::from(pool.size());
            (idle, size.saturating_sub(idle))
        }),
        // 读的是计数器的当前值，不是 [`count_pool_timeout`]（那是**递增**用的，
        // 返回 `()`）—— 闭包要的是数据源。
        Box::new(|| POOL_TIMEOUTS.load(Ordering::Relaxed)),
        // 三个 counter 都是**进程级**的，与具体池无关 —— 每个 backend 各出一份
        // 同样的值（spec:726 的既定接法，不是缺陷）。
        Box::new(|| timeout_counter(BackendKind::Rdbms).load(Ordering::Relaxed)),
        Box::new(|| TRANSACTIONS_LEAKED.load(Ordering::Relaxed)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqlxClient;
    use ecat_data::RdbmsError;

    async fn client() -> SqlxClient {
        SqlxClient::connect("sqlite::memory:").await.unwrap()
    }

    /// 抓取后按 `指标名{backend="..."}` 找样本值。找不到就是 None —— 断言
    /// 「指标压根没出现」与「值不对」是两码事，测试要能分开报。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
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
        register_pool_metrics("sqlx-sqlite-test", c.pool());

        let q_lo = timeout_counter(BackendKind::Rdbms).fetch_add(1, Ordering::Relaxed) + 1;
        let l_lo = TRANSACTIONS_LEAKED.fetch_add(1, Ordering::Relaxed) + 1;

        let text = ecat_metrics::metrics_text();

        let q_hi = timeout_counter(BackendKind::Rdbms).load(Ordering::Relaxed);
        let l_hi = TRANSACTIONS_LEAKED.load(Ordering::Relaxed);

        for name in [
            "ecat_rdbms_pool_connections",
            "ecat_rdbms_pool_timeouts_total",
            "ecat_rdbms_query_timeout_total",
            "ecat_rdbms_transactions_leaked_total",
        ] {
            assert!(
                text.contains(&format!("{name}{{backend=\"sqlx-sqlite-test\"")),
                "缺指标 {name}，实际输出:\n{text}"
            );
        }
        assert_bracketed(
            &text,
            "ecat_rdbms_query_timeout_total{backend=\"sqlx-sqlite-test\"}",
            q_lo,
            q_hi,
        );
        assert_bracketed(
            &text,
            "ecat_rdbms_transactions_leaked_total{backend=\"sqlx-sqlite-test\"}",
            l_lo,
            l_hi,
        );
    }

    /// gauge 是**抓取时现读**的：同一条连接被借出后 idle 归零、active 记为 1。
    /// 若哪天改成「注册时快照一次」，这条会红。
    #[tokio::test]
    async fn gauge_reflects_live_pool_state() {
        let c = client().await;
        register_pool_metrics("sqlx-live-test", c.pool());

        let idle = || {
            sample(
                &ecat_metrics::metrics_text(),
                "ecat_rdbms_pool_connections{backend=\"sqlx-live-test\",state=\"idle\"}",
            )
        };
        let active = || {
            sample(
                &ecat_metrics::metrics_text(),
                "ecat_rdbms_pool_connections{backend=\"sqlx-live-test\",state=\"active\"}",
            )
        };

        assert_eq!(idle(), Some(1.0), "刚建好池应当有一条空闲连接");
        assert_eq!(active(), Some(0.0));

        let guard = c.pool().acquire().await.unwrap();
        assert_eq!(idle(), Some(0.0), "连接被借出后不应再算空闲");
        assert_eq!(active(), Some(1.0));
        drop(guard);
    }

    /// 等连接超时走 `db_err` 这条唯一漏斗：它必须把 `PoolTimedOut` 计数，
    /// 而且这个数要能从 `ecat_rdbms_pool_timeouts_total` 抓出来。
    ///
    /// 这里的值可以钉精确：本测试进程里只有这条用例会推进 `POOL_TIMEOUTS`
    /// （没有别的用例制造等连接超时，另两个 counter 才是全局共享的）。
    #[tokio::test]
    async fn pool_timeout_is_counted_by_db_err() {
        let before = POOL_TIMEOUTS.load(Ordering::Relaxed);
        let err = crate::db_err(sqlx::Error::PoolTimedOut);
        assert!(matches!(err, RdbmsError::Database(_)), "got: {err:?}");
        let counted = POOL_TIMEOUTS.load(Ordering::Relaxed);
        assert_eq!(counted, before + 1);

        // 其它错误不该被算进来。
        let err = crate::db_err(sqlx::Error::RowNotFound);
        assert!(matches!(err, RdbmsError::Database(_)));
        assert_eq!(
            POOL_TIMEOUTS.load(Ordering::Relaxed),
            counted,
            "非 PoolTimedOut 不该计数"
        );

        // 记的这一笔必须出现在指标里（区间上界放在抓取之后）。
        let c = client().await;
        register_pool_metrics("sqlx-timeout-test", c.pool());
        let text = ecat_metrics::metrics_text();
        let hi = POOL_TIMEOUTS.load(Ordering::Relaxed);
        assert_bracketed(
            &text,
            "ecat_rdbms_pool_timeouts_total{backend=\"sqlx-timeout-test\"}",
            counted,
            hi,
        );
    }

    /// 同一个 backend 注册两次只应留一份样本（否则 Prometheus 会因重复样本报错）。
    #[tokio::test]
    async fn same_backend_registration_replaces_instead_of_duplicating() {
        let a = client().await;
        let b = client().await;
        register_pool_metrics("sqlx-dup-test", a.pool());
        register_pool_metrics("sqlx-dup-test", b.pool());

        let text = ecat_metrics::metrics_text();
        let hits = text
            .lines()
            .filter(|l| {
                l.starts_with(
                    "ecat_rdbms_pool_connections{backend=\"sqlx-dup-test\",state=\"idle\"}",
                )
            })
            .count();
        assert_eq!(hits, 1, "重复注册应覆盖而不是追加，实际输出:\n{text}");
    }
}

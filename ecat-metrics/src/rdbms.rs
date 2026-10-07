// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! 四个 `ecat_rdbms_*` 家族的**全进程唯一** collector（与 feature 无关）。
//!
//! 与 [`crate::outbound`] 同一个理由（见 `outbound.rs` 的模块文档与「出入 11」）：
//! `crate::registry()` 是全进程一个 `Registry`，而 `Registry` 按「指标名 + 常量
//! 标签」去重 —— **每个后端 crate 各建一份同名 collector，只会剩先注册的那份**，
//! 后注册者的四个指标一条样本都不输出（`register()` 返回 `AlreadyReg`，调用方
//! 通常 `let _ =` 掉）。批次 4 的 `ecat-data-sqlx` / `ecat-data-mssql` 正是这样
//! 互相顶掉的（Task 3 修的就是它）。
//!
//! 所以这里只建一份 collector，各后端把**取数闭包**挂进来。指标名、HELP、
//! 标签名与语义与批次 4 完全一致 —— 那是**已发布的遥测契约**。
//!
//! 为什么不直接收 `&Pool`：那会让 `ecat-metrics` 依赖 sqlx / deadpool，把两个
//! 重型驱动拖进这个基础 crate。`Box<dyn Fn>` 只要求调用方在自己的闭包里捕获池。

use prometheus::core::{Collector, Desc};
use prometheus::proto::{Counter, Gauge, LabelPair, Metric, MetricFamily, MetricType};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// 取连接池的 `(idle, active)`。
///
/// 两个数**一起**返回（而不是两个闭包）：它们是同一次池状态读取的两个投影，
/// 分开取会在并发下读到不同瞬间的池，产出 `idle + active > size` 的假样本。
pub type RdbmsConnectionsFn = Box<dyn Fn() -> (u64, u64) + Send + Sync>;

/// 读一个进程级计数器的当前值。
pub type RdbmsCounterFn = Box<dyn Fn() -> u64 + Send + Sync>;

/// 把一个后端的数据源挂到全进程唯一的 collector 上。
///
/// `backend` 是标签值（`"primary"` / `"replica-1"` 之类）。同一个 `backend` 重复
/// 注册**替换**（不叠加）：同一标签出现两份样本会被抓取端判为重复。
pub fn register_rdbms_metrics(
    backend: &'static str,
    connections: RdbmsConnectionsFn,
    pool_timeouts: RdbmsCounterFn,
    query_timeouts: RdbmsCounterFn,
    transactions_leaked: RdbmsCounterFn,
) {
    let rdbms = RDBMS.get_or_init(|| {
        let rdbms = Arc::new(Rdbms::new());
        // 与 ecat-metrics 自己的注册同款：AlreadyReg 只可能是名字撞车，而这里的
        // 名字是全仓独占的（本函数是这四个家族唯一的写入口）。
        let _ = crate::registry().register(Box::new(Registered(Arc::clone(&rdbms))));
        rdbms
    });
    rdbms.add(Entry {
        backend,
        connections,
        pool_timeouts,
        query_timeouts,
        transactions_leaked,
    });
}

/// 真正注册进 registry 的那层壳。
///
/// 不能直接 `impl Collector for Arc<Rdbms>`：`Arc` 是外部类型，孤儿规则不允许
/// （E0117）。包一层自有类型即可，内部仍是同一个 [`Rdbms`]。
struct Registered(Arc<Rdbms>);

impl Collector for Registered {
    fn desc(&self) -> Vec<&Desc> {
        self.0.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        self.0.collect()
    }
}

static RDBMS: OnceLock<Arc<Rdbms>> = OnceLock::new();

/// 一个 backend 的数据源。
struct Entry {
    backend: &'static str,
    connections: RdbmsConnectionsFn,
    pool_timeouts: RdbmsCounterFn,
    query_timeouts: RdbmsCounterFn,
    transactions_leaked: RdbmsCounterFn,
}

struct Rdbms {
    entries: Mutex<Vec<Entry>>,
    descs: [Desc; 4],
}

impl Rdbms {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
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

    fn add(&self, entry: Entry) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.iter_mut().find(|e| e.backend == entry.backend) {
            Some(slot) => *slot = entry,
            None => entries.push(entry),
        }
    }
}

impl Collector for Rdbms {
    fn desc(&self) -> Vec<&Desc> {
        self.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut connections = Vec::with_capacity(entries.len() * 2);
        let mut pool_timeouts = Vec::with_capacity(entries.len());
        let mut query_timeouts = Vec::with_capacity(entries.len());
        let mut leaked = Vec::with_capacity(entries.len());

        // 闭包在这里被调 —— 取数发生在**抓取时**，不是注册时。
        for e in entries.iter() {
            let backend = e.backend;
            let (idle, active) = (e.connections)();
            connections.push(gauge(
                labels(&[("backend", backend), ("state", "idle")]),
                count(idle),
            ));
            connections.push(gauge(
                labels(&[("backend", backend), ("state", "active")]),
                count(active),
            ));
            pool_timeouts.push(counter(
                labels(&[("backend", backend)]),
                count((e.pool_timeouts)()),
            ));
            query_timeouts.push(counter(
                labels(&[("backend", backend)]),
                count((e.query_timeouts)()),
            ));
            leaked.push(counter(
                labels(&[("backend", backend)]),
                count((e.transactions_leaked)()),
            ));
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
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 抓取后按 `指标名{backend="..."` 找样本值。找不到是 None —— 断言「压根没
    /// 出现」与「值不对」是两码事，测试要能分开报。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 取数闭包是**抓取时**调用的 —— 注册时快照一次的实现会停在旧值上。
    /// 各用例用**互不相同**的 backend 标签：本模块的静态量与 registry 都是进程级的。
    #[test]
    fn metrics_are_read_live_at_scrape_time() {
        static IDLE: AtomicU64 = AtomicU64::new(7);
        register_rdbms_metrics(
            "rdbms-live-test",
            Box::new(move || (IDLE.load(Ordering::Relaxed), 0u64)),
            Box::new(|| 0u64),
            Box::new(|| 0u64),
            Box::new(|| 0u64),
        );

        let read = || {
            sample(
                &crate::metrics_text(),
                "ecat_rdbms_pool_connections{backend=\"rdbms-live-test\",state=\"idle\"}",
            )
        };
        assert_eq!(read(), Some(7.0));
        IDLE.store(9, Ordering::Relaxed);
        assert_eq!(read(), Some(9.0), "注册时快照的实现会停在上一次的值");
    }

    /// 多个 backend 共存时**谁都不丢** —— 这是本模块存在的唯一理由
    /// （「每 crate 一份 collector」的老做法在这里必红）。
    #[test]
    fn multiple_backends_coexist_in_one_registry() {
        for (backend, base) in [("rdbms-multi-a", 1u64), ("rdbms-multi-b", 4u64)] {
            register_rdbms_metrics(
                backend,
                Box::new(move || (base, 0u64)),
                Box::new(move || base),
                Box::new(move || base),
                Box::new(move || base),
            );
        }

        let text = crate::metrics_text();
        for backend in ["rdbms-multi-a", "rdbms-multi-b"] {
            for family in [
                "ecat_rdbms_pool_connections",
                "ecat_rdbms_pool_timeouts_total",
                "ecat_rdbms_query_timeout_total",
                "ecat_rdbms_transactions_leaked_total",
            ] {
                assert!(
                    text.contains(&format!("{family}{{backend=\"{backend}\"")),
                    "缺 {family}{{backend=\"{backend}\"}}，实际输出:\n{text}"
                );
            }
        }
    }

    /// 同一个 backend 重复注册**替换**而不是叠加：同一标签出现两份样本会被抓取端判为重复。
    #[test]
    fn same_backend_registration_replaces_instead_of_duplicating() {
        for value in [1u64, 2u64] {
            register_rdbms_metrics(
                "rdbms-dup-test",
                Box::new(move || (value, 0u64)),
                Box::new(move || value),
                Box::new(move || value),
                Box::new(move || value),
            );
        }

        let text = crate::metrics_text();
        let hits = text
            .lines()
            .filter(|l| l.contains("backend=\"rdbms-dup-test\""))
            .count();
        assert_eq!(
            hits, 5,
            "2 条 gauge（idle/active）+ 3 条 counter，一份不少一份不多:\n{text}"
        );
    }
}

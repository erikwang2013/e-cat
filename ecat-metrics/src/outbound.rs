// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! 出站调用的三个指标（spec §4）：
//!
//! | 指标 | 类型 | 维度 |
//! |---|---|---|
//! | `ecat_outbound_timeouts_total` | counter | `backend` |
//! | `ecat_outbound_breaker_open_total` | counter | `backend` |
//! | `ecat_outbound_breaker_state` | gauge | `backend`（0=closed 1=open 2=half-open）|
//!
//! **为什么这三个指标家族在本模块，而不是像批次 4 那样放在各后端 crate 里**：
//! [`crate::registry()`] 是全进程一个 `Registry`，而它按「指标名 + 常量标签」去重
//! —— 同名 `Collector` 注册第二次直接 `AlreadyReg`，之后那个 collector 的样本
//! **一个都不输出**（`ecat-data-sqlx/src/metrics.rs:43-49` 写明了这个后果）。
//! 这三个名字是全进程共享的命名空间，而 5b 之后会有 14 个后端同时注册它们：
//! 「每 crate 一份 collector」会变成「谁先注册谁独活，其余静默消失」，
//! 而且单 crate 跑测试还看不见。所以指标家族在这里**只建一份**，各后端只挂数据源。
//!
//! 三项都**抓取时现读**：注册时快照一次没有意义，指标的价值就在随状态变。
//!
//! 数据源用 `Box<dyn Fn>` 而不直接收 `Breaker`：本 crate 因此不必依赖
//! `ecat-circuit-breaker`（拖 tower）与 `ecat-data`（拖 tokio + async-trait），
//! 保住现有「只有 prometheus + axum」的窄依赖树。

use crate::registry;
use prometheus::core::{Collector, Desc};
use prometheus::proto::{Counter, Gauge, LabelPair, Metric, MetricFamily, MetricType};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// 抓取时取一个 counter 值的闭包。
pub type OutboundCounterFn = Box<dyn Fn() -> u64 + Send + Sync>;

/// 抓取时取熔断状态的闭包：`0` = closed、`1` = open、`2` = half-open。
///
/// 编码由 `ecat_circuit_breaker::BreakerState::code()` 提供，本 crate 不知道
/// `BreakerState` 的存在。
pub type OutboundStateFn = Box<dyn Fn() -> u8 + Send + Sync>;

/// 挂一个后端的出站数据源。`backend` 是标签值（`"redis"` / `"clickhouse"` 之类）。
///
/// 幂等：collector 只注册一次；同一个 `backend` 重复注册则**覆盖**它的三个闭包
/// （避免同一标签出现两份样本 —— 那样 Prometheus 会因重复样本报错）。
///
/// 唯一会失败的情形是 registry 里已有同名指标 —— 那时三个指标都不会输出。
pub fn register_outbound_metrics(
    backend: &'static str,
    timeouts: OutboundCounterFn,
    breaker_opened: OutboundCounterFn,
    breaker_state: OutboundStateFn,
) {
    let outbound = OUTBOUND.get_or_init(|| {
        let outbound = Arc::new(Outbound::new());
        // 与 ecat-metrics 自己的注册同款：AlreadyReg 只可能是名字撞车，
        // 而这三个名字以 ecat_outbound_ 独占。
        let _ = registry().register(Box::new(Registered(Arc::clone(&outbound))));
        outbound
    });

    let entry = Entry {
        backend,
        timeouts,
        breaker_opened,
        breaker_state,
    };
    let mut entries = outbound.entries.lock().unwrap();
    match entries.iter_mut().find(|e| e.backend == backend) {
        Some(slot) => *slot = entry,
        None => entries.push(entry),
    }
}

/// 真正注册进 registry 的那层壳。
///
/// 不能直接 `impl Collector for Arc<Outbound>`：`Arc` 是外部类型，孤儿规则不允许
/// （E0117）。包一层自有类型即可，内部仍是同一个 [`Outbound`]。
struct Registered(Arc<Outbound>);

impl Collector for Registered {
    fn desc(&self) -> Vec<&Desc> {
        self.0.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        self.0.collect()
    }
}

/// 全局 collector：一个进程一个，三个指标家族都在它身上。
static OUTBOUND: OnceLock<Arc<Outbound>> = OnceLock::new();

/// 一个后端挂上来的三个数据源。
struct Entry {
    backend: &'static str,
    timeouts: OutboundCounterFn,
    breaker_opened: OutboundCounterFn,
    breaker_state: OutboundStateFn,
}

struct Outbound {
    entries: Mutex<Vec<Entry>>,
    descs: [Desc; 3],
}

impl Outbound {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            descs: [
                desc(
                    "ecat_outbound_timeouts_total",
                    "Total outbound call timeouts",
                    &["backend"],
                ),
                desc(
                    "ecat_outbound_breaker_open_total",
                    "Total circuit breaker openings",
                    &["backend"],
                ),
                desc(
                    "ecat_outbound_breaker_state",
                    "Circuit breaker state (0=closed 1=open 2=half-open)",
                    &["backend"],
                ),
            ],
        }
    }
}

impl Collector for Outbound {
    fn desc(&self) -> Vec<&Desc> {
        self.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let entries = self.entries.lock().unwrap();
        let mut timeouts = Vec::with_capacity(entries.len());
        let mut opened = Vec::with_capacity(entries.len());
        let mut states = Vec::with_capacity(entries.len());

        for e in entries.iter() {
            let one = labels(&[("backend", e.backend)]);
            timeouts.push(counter(one.clone(), count((e.timeouts)())));
            opened.push(counter(one.clone(), count((e.breaker_opened)())));
            states.push(gauge(one, f64::from((e.breaker_state)())));
        }

        vec![
            family(
                "ecat_outbound_timeouts_total",
                "Total outbound call timeouts",
                MetricType::COUNTER,
                timeouts,
            ),
            family(
                "ecat_outbound_breaker_open_total",
                "Total circuit breaker openings",
                MetricType::COUNTER,
                opened,
            ),
            family(
                "ecat_outbound_breaker_state",
                "Circuit breaker state (0=closed 1=open 2=half-open)",
                MetricType::GAUGE,
                states,
            ),
        ]
    }
}

fn count(v: u64) -> f64 {
    v as f64
}

/// 下面五个函数与 `ecat-data-sqlx/src/metrics.rs:190-237` 逐字相同。
///
/// 是**故意重复**而不是提到本 crate 里共用：那边是 feature 门控下的私有辅助函数，
/// 共用要把它们变成 `ecat-metrics` 的公开 API，而它们只是 prometheus 的结构体
/// 拼装，公开没有价值。
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

    /// 抓取后按 `指标名{backend="..."}` 找样本值。找不到就是 None —— 断言
    /// 「指标压根没出现」与「值不对」是两码事，测试要能分开报。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且值**直读数据源**：数据源在**注册之后**才推进，
    /// 快照型实现只能给出 0。
    #[test]
    fn metrics_are_read_live_at_scrape_time() {
        static TIMEOUTS: AtomicU64 = AtomicU64::new(0);
        let opened = Arc::new(AtomicU64::new(0));
        let state = Arc::new(AtomicU64::new(0));

        let o = Arc::clone(&opened);
        let s = Arc::clone(&state);
        register_outbound_metrics(
            "live-test",
            Box::new(|| TIMEOUTS.load(Ordering::Relaxed)),
            Box::new(move || o.load(Ordering::Relaxed)),
            Box::new(move || s.load(Ordering::Relaxed) as u8),
        );

        // 注册之后才推进。
        TIMEOUTS.store(7, Ordering::Relaxed);
        opened.store(3, Ordering::Relaxed);
        state.store(2, Ordering::Relaxed);

        let text = crate::metrics_text();
        assert_eq!(
            sample(&text, "ecat_outbound_timeouts_total{backend=\"live-test\"}"),
            Some(7.0),
            "{text}"
        );
        assert_eq!(
            sample(
                &text,
                "ecat_outbound_breaker_open_total{backend=\"live-test\"}"
            ),
            Some(3.0),
            "{text}"
        );
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_state{backend=\"live-test\"}"),
            Some(2.0),
            "{text}"
        );
    }

    /// **多个后端同时注册，样本必须都在。**
    ///
    /// 这条是「collector 必须全进程一份」的验收（出入 11）：照批次 4 的
    /// 「每 crate 一份 collector」写法，第二个注册者会拿到 `AlreadyReg`，
    /// 它的样本一个都不出现 —— 这条会红。
    #[test]
    fn multiple_backends_coexist_in_one_registry() {
        register_outbound_metrics("multi-a", Box::new(|| 1), Box::new(|| 2), Box::new(|| 0));
        register_outbound_metrics("multi-b", Box::new(|| 11), Box::new(|| 22), Box::new(|| 1));

        let text = crate::metrics_text();
        for (backend, expect) in [("multi-a", 1.0), ("multi-b", 11.0)] {
            let prefix = format!("ecat_outbound_timeouts_total{{backend=\"{backend}\"}}");
            assert_eq!(
                sample(&text, &prefix),
                Some(expect),
                "缺 {prefix}，实际输出:\n{text}"
            );
        }
    }

    /// 同一个 backend 注册两次只留一份样本（否则 Prometheus 会因重复样本报错），
    /// 且留下的是**新的闭包**。
    #[test]
    fn same_backend_registration_replaces_instead_of_duplicating() {
        register_outbound_metrics("dup-test", Box::new(|| 1), Box::new(|| 0), Box::new(|| 0));
        register_outbound_metrics("dup-test", Box::new(|| 9), Box::new(|| 0), Box::new(|| 0));

        let text = crate::metrics_text();
        let hits = text
            .lines()
            .filter(|l| l.starts_with("ecat_outbound_timeouts_total{backend=\"dup-test\"}"))
            .count();
        assert_eq!(hits, 1, "重复注册应覆盖而不是追加，实际输出:\n{text}");
        assert_eq!(
            sample(&text, "ecat_outbound_timeouts_total{backend=\"dup-test\"}"),
            Some(9.0),
            "留下的必须是新闭包"
        );
    }
}

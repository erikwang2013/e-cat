// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 neo4j 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族（`ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total`
//! / `ecat_outbound_breaker_state`）的 collector **不在本 crate** —— 指标名是全进程
//! 共享的命名空间，每个后端各建一份会在 `Registry` 里撞名（`AlreadyReg`），让后
//! 注册者的样本静默消失。理由见批次 5a 的「出入 11」与 `ecat-metrics/src/outbound.rs`。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 测试期串行锁：`from_config` 在 `metrics` feature 下**构造即注册**，而注册是
/// **覆盖**语义 —— 同一测试二进制里每个 `client_at`（= 每个延迟/并发用例）都会走
/// 一遍 `from_config`，观察者用例跑到一半被覆盖就会读到别人的 breaker。
/// （实测：opensearch 二进制连跑 40 次，`outbound_metrics_appear_with_live_values`
/// 红 12 次，断言全是 open_total 0 ≠ 1。）写者（`from_config` 里）与观察者都取本锁
/// ⇒ 观察窗内无并发写者。**仅测试构建存在**，生产无锁、无此代码。
#[cfg(test)]
static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 取 [`TEST_SERIAL`]；中毒时取回内部值（一条用例 panic 不该连锁带红其余用例）。
#[cfg(test)]
pub(crate) fn lock_test_serial() -> std::sync::MutexGuard<'static, ()> {
    TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// 挂上 neo4j 的出站数据源。标签值固定为 `"neo4j"`（= 配置节名）。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        "neo4j",
        Box::new(|| timeout_counter(BackendKind::Graph).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Neo4jClient, Neo4jConfig};
    use ecat_circuit_breaker::{BreakerConfig, BreakerState};

    /// 抓取后按 `指标名{backend="neo4j"}` 找样本值（找不到 = None）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且**值是抓取时现读的**：先 +1000 再抓，快照实现只会给出 0。
    ///
    /// `timeout_counter(BackendKind::Graph)` 的 kind 误接（写成别的槽）在这里红：
    /// 两份样本都会存在，但值差着 1000。
    /// **本用例只推进 Graph 槽**（= 本 crate 自己的 kind），证人槽（Storage）纹丝不动。
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        // 观察窗 1（持锁）：注册 → 抓取之间没有别的写者（见 `TEST_SERIAL`）。
        let _serial = lock_test_serial();
        register_outbound_metrics(Arc::clone(&breaker));

        timeout_counter(BackendKind::Graph).fetch_add(1000, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        assert!(
            sample(&text, r#"ecat_outbound_timeouts_total{backend="neo4j"}"#)
                .is_some_and(|v| v >= 1000.0),
            "超时样本应现读静态量（先 +1000 再抓取），实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="neo4j"}"#),
            Some(0.0),
            "未打开时状态应为 0，实际输出:\n{text}"
        );

        // 放锁再 await：`breaker.call` 是 async，持 std `MutexGuard` 跨 await 会被
        // `clippy::await_holding_lock` 按 `-D warnings` 拦下。本用例不需要在这段
        // 持锁 —— 下面抓取前会重新取锁。
        drop(_serial);
        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);
        // 观察窗 2（持锁）：窗口 1 之后条目可能被别人覆盖过，这里先**覆盖回自己**
        // 那台 breaker 再抓取 —— 注册与抓取之间不 await（见上）。
        let _serial = lock_test_serial();
        register_outbound_metrics(Arc::clone(&breaker));
        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(
                &text,
                r#"ecat_outbound_breaker_open_total{backend="neo4j"}"#
            ),
            Some(1.0),
            "打开次数应为 1，实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="neo4j"}"#),
            Some(1.0),
            "打开后状态应为 1（现读），实际输出:\n{text}"
        );
    }

    /// **`from_config` 自动注册**（lead 裁决 2026-10-08）：构造即接线，
    /// 不需要用户额外调用。**不发任何请求** —— 只构造再抓一次指标文本。
    ///
    /// 探针：注释掉 `from_config` 里那两行 ⇒ 本用例红。**必须用测试名过滤单独跑**
    /// （`cargo test -p <crate> --features metrics from_config_registers_outbound_metrics`）：
    /// 同二进制的 `outbound_metrics_appear_with_live_values` 会显式注册**同一个标签**，
    /// 全量跑时它先把标签挂上就掩盖了本探针（已实测：全量跑时探针不红）。
    #[tokio::test]
    async fn from_config_registers_outbound_metrics() {
        let cfg: Neo4jConfig = serde_json::from_str(
            r#"{"base_url":"http://127.0.0.1:1","username":"u","password":"p"}"#,
        )
        .unwrap();
        let _c = Neo4jClient::from_config(cfg).unwrap();
        let text = ecat_metrics::metrics_text();
        assert!(
            text.contains(r#"ecat_outbound_timeouts_total{backend="neo4j"}"#),
            "from_config 没自动注册？实际输出:\n{text}"
        );
    }
}

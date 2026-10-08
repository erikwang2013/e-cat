// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 nebulagraph 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族（`ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total`
//! / `ecat_outbound_breaker_state`）的 collector **不在本 crate** —— 指标名是全进程
//! 共享的命名空间，每个后端各建一份会在 `Registry` 里撞名（`AlreadyReg`），让后
//! 注册者的样本静默消失。理由见批次 5a 的「出入 11」与 `ecat-metrics/src/outbound.rs`。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 nebulagraph 的出站数据源。标签值固定为 `"nebulagraph"`（= 配置节名）。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    register_as("nebulagraph", breaker);
}

/// 挂到任意标签上 —— **只给测试用**：注册表按标签**覆盖**（`ecat-metrics/src/outbound.rs`），
/// 而 `metrics` feature 下每个 `from_config` 都会往公共标签上再写一次，所以
/// 「断言自己那台熔断器」的用例必须用**私有标签**，否则是在赌「谁最后注册」
/// （`ecat-metrics` 自己的用例用的是同一个路子）。生产入口固定 `"nebulagraph"`。
fn register_as(backend: &'static str, breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        backend,
        Box::new(|| timeout_counter(BackendKind::Graph).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NebulaGraphClient, NebulaGraphConfig};
    use ecat_circuit_breaker::{BreakerConfig, BreakerState};

    /// 抓取后按 `指标名{backend="nebulagraph-live-test"}` 找样本值（找不到 = None）。
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
    ///
    /// 注册用**私有标签** `nebulagraph-live-test`：公共标签 `nebulagraph` 在 `metrics`
    /// feature 下被同二进制的每个 `from_config` 反复覆盖（自动注册），拿它断言
    /// 具体值就是在赌「谁最后注册」。公共标签的接线由
    /// `from_config_registers_outbound_metrics` 断言「样本存在」（与注册者是谁无关）。
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_as("nebulagraph-live-test", Arc::clone(&breaker));

        timeout_counter(BackendKind::Graph).fetch_add(1000, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        assert!(
            sample(
                &text,
                r#"ecat_outbound_timeouts_total{backend="nebulagraph-live-test"}"#
            )
            .is_some_and(|v| v >= 1000.0),
            "超时样本应现读静态量（先 +1000 再抓取），实际输出:\n{text}"
        );
        assert_eq!(
            sample(
                &text,
                r#"ecat_outbound_breaker_state{backend="nebulagraph-live-test"}"#
            ),
            Some(0.0),
            "未打开时状态应为 0，实际输出:\n{text}"
        );

        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);
        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(
                &text,
                r#"ecat_outbound_breaker_open_total{backend="nebulagraph-live-test"}"#
            ),
            Some(1.0),
            "打开次数应为 1，实际输出:\n{text}"
        );
        assert_eq!(
            sample(
                &text,
                r#"ecat_outbound_breaker_state{backend="nebulagraph-live-test"}"#
            ),
            Some(1.0),
            "打开后状态应为 1（现读），实际输出:\n{text}"
        );
    }

    /// **`from_config` 自动注册**（lead 裁决 2026-10-08）：构造即接线，
    /// 不需要用户额外调用。**不发任何请求** —— 只构造再抓一次指标文本。
    ///
    /// 探针：注释掉 `from_config` 里那条注册语句 ⇒ 本用例红。观察者用例改用私有
    /// 标签 `nebulagraph-live-test` 后，公共标签在本测试二进制里没有别的注册者，
    /// **全量跑也一样红**（实测 7/7：过滤与全量均 rc=101），不必再按测试名过滤。
    #[tokio::test]
    async fn from_config_registers_outbound_metrics() {
        let cfg: NebulaGraphConfig =
            serde_json::from_str(r#"{"base_url":"http://127.0.0.1:1","space":"s"}"#).unwrap();
        let _c = NebulaGraphClient::from_config(cfg).unwrap();
        let text = ecat_metrics::metrics_text();
        assert!(
            text.contains(r#"ecat_outbound_timeouts_total{backend="nebulagraph"}"#),
            "from_config 没自动注册？实际输出:\n{text}"
        );
    }
}

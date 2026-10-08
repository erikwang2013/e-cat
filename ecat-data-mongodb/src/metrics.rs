// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 mongodb 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族（`ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total`
//! / `ecat_outbound_breaker_state`）的 collector **不在本 crate** —— 指标名是全进程
//! 共享的命名空间，每个后端各建一份会在 `Registry` 里撞名（`AlreadyReg`），让后
//! 注册者的样本静默消失。理由见批次 5a 的「出入 11」与 `ecat-metrics/src/outbound.rs`。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 mongodb 的出站数据源。标签值固定为 `"mongodb"`（= 配置节名）。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        "mongodb",
        Box::new(|| timeout_counter(BackendKind::Document).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::{BreakerConfig, BreakerState};

    /// 抓取后按 `指标名{backend="mongodb"}` 找样本值（找不到 = None）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且**值是抓取时现读的**：先 +1000 再抓，快照实现只会给出 0。
    ///
    /// `timeout_counter(BackendKind::Document)` 的 kind 误接（写成别的槽）在这里红：
    /// 两份样本都会存在，但值差着 1000。
    /// **本用例只推进 Document 槽**（= 本 crate 自己的 kind），证人槽（Storage）纹丝不动。
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_outbound_metrics(Arc::clone(&breaker));

        timeout_counter(BackendKind::Document).fetch_add(1000, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        assert!(
            sample(&text, r#"ecat_outbound_timeouts_total{backend="mongodb"}"#)
                .is_some_and(|v| v >= 1000.0),
            "超时样本应现读静态量（先 +1000 再抓取），实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="mongodb"}"#),
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
                r#"ecat_outbound_breaker_open_total{backend="mongodb"}"#
            ),
            Some(1.0),
            "打开次数应为 1，实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="mongodb"}"#),
            Some(1.0),
            "打开后状态应为 1（现读），实际输出:\n{text}"
        );
    }
}

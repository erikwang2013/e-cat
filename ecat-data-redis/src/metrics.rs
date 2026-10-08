// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 Redis 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族（`ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total`
//! / `ecat_outbound_breaker_state`）的 collector **不在本 crate** —— 指标名是全进程
//! 共享的命名空间，每个后端各建一份会在 `Registry` 里撞名（`AlreadyReg`），让后
//! 注册者的样本静默消失。理由见批次 5a 的「出入 11」与 `ecat-metrics/src/outbound.rs`。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 Redis 的出站数据源。标签值固定为 `"redis"`。
///
/// 收的是**熔断器**而不是 `RedisCache`：三项数据源都与具体实例无关 ——
/// 超时数读的是按**后端类别**的进程级静态量（`TIMEOUTS` 的 `Cache` 槽），
/// 熔断两项读的是这个熔断器本身。所以不必持有整个 client，测试也就能
/// 不依赖真实连接地验证注册。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        "redis",
        Box::new(|| timeout_counter(BackendKind::Cache).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerState};

    /// 抓取后按 `指标名{backend="redis"}` 找样本值。找不到就是 None ——
    /// 「指标压根没出现」与「值不对」必须能分开报（`contains("} 0")` 会把
    /// `} 0.5` 也算进去，所以不能只做字符串包含断言）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且**值是抓取时现读的**：熔断器在**注册之后**才被推到
    /// `Open`，注册时快照的实现只会给出 0。
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_outbound_metrics(Arc::clone(&breaker));

        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_state{backend=\"redis\"}"),
            Some(0.0),
            "未打开时状态应为 0，实际输出:\n{text}"
        );

        // 打满窗口的样本下限（5 条失败）→ 打开。
        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);

        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_state{backend=\"redis\"}"),
            Some(1.0),
            "打开后状态应为 1，实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_open_total{backend=\"redis\"}"),
            Some(1.0),
            "打开次数应为 1，实际输出:\n{text}"
        );
        // 超时数是进程级静态量，值会被别的用例推进 —— 只断言样本存在。
        assert!(
            sample(&text, "ecat_outbound_timeouts_total{backend=\"redis\"}").is_some(),
            "缺超时指标样本，实际输出:\n{text}"
        );
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics": wire Cassandra's outbound data source into
//! `ecat-metrics`' shared collector.
//!
//! The collector for the three metric families lives in `ecat-metrics`, not
//! here: metric names are process-wide, so one collector per backend would
//! collide in the `Registry` (`AlreadyReg`) and silently drop samples.

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Register Cassandra's outbound source. The label is fixed to `"cassandra"`
/// (the config section name).
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    register_as("cassandra", breaker);
}

/// Register under an arbitrary label — **tests only**: the registry overwrites
/// by label, and every `from_config` under the `metrics` feature writes the
/// public label again, so a test asserting a specific breaker's values must use
/// a private label. Production always uses `"cassandra"`.
fn register_as(backend: &'static str, breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        backend,
        Box::new(|| timeout_counter(BackendKind::Rdbms).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::{BreakerConfig, BreakerState};

    /// Find a sample value by `metric{backend="..."}` prefix (None = absent).
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|line| line.starts_with(prefix) && !line.starts_with('#'))
            .and_then(|line| line.rsplit(' ').next())
            .and_then(|value| value.parse().ok())
    }

    /// All three metrics must appear and read **live** values: +1000 before the
    /// scrape means a snapshot implementation would report 0.
    ///
    /// Private label `cassandra-live-test`: the public label is overwritten by
    /// every `from_config` in the same binary, so asserting its value would be
    /// a race. This test also advances only the `Rdbms` slot (its own kind).
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_as("cassandra-live-test", Arc::clone(&breaker));

        timeout_counter(BackendKind::Rdbms).fetch_add(1000, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        assert!(
            sample(
                &text,
                r#"ecat_outbound_timeouts_total{backend="cassandra-live-test"}"#
            )
            .is_some_and(|value| value >= 1000.0),
            "timeout sample should be read live, output:\n{text}"
        );
        assert_eq!(
            sample(
                &text,
                r#"ecat_outbound_breaker_state{backend="cassandra-live-test"}"#
            ),
            Some(0.0),
            "breaker state should be closed, output:\n{text}"
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
                r#"ecat_outbound_breaker_open_total{backend="cassandra-live-test"}"#
            ),
            Some(1.0),
            "open count should be 1, output:\n{text}"
        );
        assert_eq!(
            sample(
                &text,
                r#"ecat_outbound_breaker_state{backend="cassandra-live-test"}"#
            ),
            Some(1.0),
            "breaker state should be open, output:\n{text}"
        );
    }
}

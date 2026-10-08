// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 ClickHouse 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族的 collector **不在本 crate**（理由见批次 5a「出入 11」与
//! `ecat-metrics/src/outbound.rs`）：本 crate 有**两条 I/O 路径**
//! （`SqlExecutor` → `Rdbms` 槽、`TsdbClient` → `Tsdb` 槽），超时计数因此出
//! **两份样本**：`backend="clickhouse"` 与 `backend="clickhouse-tsdb"`。
//! 熔断两项是同一个 `Breaker`（两条路径共用，见 `ClickhouseClient` 的字段说明），
//! 所以两份样本的值相同 —— 这是有意的，方便按 `backend` 分组时两条都看得到。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 ClickHouse 的出站数据源。收熔断器而非整个 client（同 Task 4）。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    register_as("clickhouse", "clickhouse-tsdb", breaker);
}

/// 两个标签**只给测试用**：注册表按标签**覆盖**
/// （`ecat-metrics/src/outbound.rs`），而 `metrics` feature 下每个 `from_config`
/// 都会往公共标签上再写一次，所以「断言自己那台熔断器」的用例必须用**私有标签**，
/// 否则是在赌「谁最后注册」（`ecat-metrics` 自己的用例用的是同一个路子）。
/// 生产入口固定 `"clickhouse"` 与 `"clickhouse-tsdb"`。
fn register_as(rdbms_backend: &'static str, tsdb_backend: &'static str, breaker: Arc<Breaker>) {
    register_one(rdbms_backend, BackendKind::Rdbms, Arc::clone(&breaker));
    register_one(tsdb_backend, BackendKind::Tsdb, breaker);
}

fn register_one(backend: &'static str, kind: BackendKind, breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        backend,
        Box::new(move || timeout_counter(kind).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerState};
    use ecat_data::TIMEOUTS;

    /// 抓取后按 `指标名{backend="..."}` 找样本值（找不到 = None）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// **两个超时维度各出一份样本** —— 只注册 `"clickhouse"` 一个标签的实现
    /// 在这里红。两条路径的超时不该合成一个数：合成后就分不清是 SQL 慢还是
    /// 时序写入慢。
    ///
    /// **本用例只推进 `Rdbms` 槽、不推进 `Tsdb` 槽**：`Tsdb` 槽必须保持「本进程
    /// 内只有一个写者」，resilience 的 `tsdb_path_counts_its_own_dimension` 才能
    /// 断言严格 `== +1`（否则这里的 +1 会落进它的观测窗口，把误路由掩护成绿）。
    /// 现读（live-read）只需在一条路径上验证：两份样本走同一个 collector、
    /// 同一段注册代码，与路径数无关。
    ///
    /// 另一件由本用例抓的事：`register_one("clickhouse-tsdb", …)` 的 kind 误接
    /// （写成 `Rdbms`）—— 误接时两份样本同源、值相等，`tsdb < clickhouse` 红。
    /// 这条轴 `guarded_tsdb` 的严格断言够不到（那是另一行代码）。
    ///
    /// 注册用**私有标签**（`-live-test` 后缀）：公共标签在 `metrics` feature 下
    /// 被同二进制的每个 `from_config` 反复覆盖（自动注册），拿它断言具体值就是
    /// 在赌「谁最后注册」—— 实测未用私有标签时（500 次）本用例红 4 次。
    /// 公共标签的接线由 `from_config_registers_outbound_metrics` 断言「样本存在」
    /// （与注册者是谁无关）。
    #[tokio::test]
    async fn both_paths_publish_their_own_timeout_sample() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_as(
            "clickhouse-live-test",
            "clickhouse-tsdb-live-test",
            Arc::clone(&breaker),
        );

        // 推进**一大格**（不是一格）：1000 与「Tsdb 槽单写者（≤1）」拉开距离，
        // 于是「两份样本同源」可直接用 `tsdb < clickhouse` 抓 —— 不依赖写者次数。
        TIMEOUTS[BackendKind::Rdbms as usize].fetch_add(1000, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        let clickhouse = sample(
            &text,
            r#"ecat_outbound_timeouts_total{backend="clickhouse-live-test"}"#,
        );
        let tsdb = sample(
            &text,
            r#"ecat_outbound_timeouts_total{backend="clickhouse-tsdb-live-test"}"#,
        );
        assert!(
            clickhouse.is_some_and(|v| v >= 1000.0),
            "clickhouse 的样本应现读静态量（先 +1000 再抓取），实际输出:\n{text}"
        );
        assert!(
            tsdb.is_some(),
            "缺 clickhouse-tsdb 的超时样本（只注册一个标签的实现在这里红），实际输出:\n{text}"
        );
        assert!(
            tsdb < clickhouse,
            "两份样本同源（tsdb 标签读了 Rdbms 槽？）—— 实际输出:\n{text}"
        );
        for backend in ["clickhouse-live-test", "clickhouse-tsdb-live-test"] {
            assert_eq!(
                sample(
                    &text,
                    &format!("ecat_outbound_breaker_state{{backend=\"{backend}\"}}")
                ),
                Some(0.0),
                "缺 {backend} 的状态样本"
            );
        }

        // 推到 Open：**两份**样本都要变成 1（同一个熔断器）。
        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);
        let text = ecat_metrics::metrics_text();
        for backend in ["clickhouse-live-test", "clickhouse-tsdb-live-test"] {
            assert_eq!(
                sample(
                    &text,
                    &format!("ecat_outbound_breaker_state{{backend=\"{backend}\"}}")
                ),
                Some(1.0),
                "{backend} 的状态应随熔断器实时变化，实际输出:\n{text}"
            );
        }
    }

    /// **`from_config` 自动注册**（lead 裁决 2026-10-08）：构造即接线，不需要用户额外调用。
    /// **不发任何请求** —— 只构造再抓一次指标文本。两条路径的两个标签都该出现。
    ///
    /// 断言的是**公共标签的样本存在**（与注册者是谁无关），所以同二进制里别的
    /// `from_config` 覆盖同一个标签不影响本用例。
    ///
    /// 探针：注释掉 `from_config` 里那两行 ⇒ 本用例红。
    #[tokio::test]
    async fn from_config_registers_outbound_metrics() {
        let cfg: crate::ClickhouseConfig =
            serde_json::from_str(r#"{"base_url":"http://127.0.0.1:1"}"#).unwrap();
        let _c = crate::ClickhouseClient::from_config(cfg).unwrap();
        let text = ecat_metrics::metrics_text();
        for backend in ["clickhouse", "clickhouse-tsdb"] {
            assert!(
                text.contains(&format!(
                    r#"ecat_outbound_timeouts_total{{backend="{backend}"}}"#
                )),
                "from_config 没自动注册？实际输出:\n{text}"
            );
        }
    }
}

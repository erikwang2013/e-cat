// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。独立成文件：`tests.rs` 已有 425 行。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::TIMEOUTS;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 一个「先拖住再回」的 mock：`delay` 之后回 200 空体。
/// 既有的 `spawn_mock` 第 4 个参数是响应头、**不是延迟**，故另起一个。
async fn spawn_slow_clickhouse(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let in_flight = Arc::clone(&in_flight);
        async move {
            in_flight.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            axum::response::Response::new(axum::body::Body::from(""))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 「N 个请求必须**同时**到齐才放行」的 mock：`n` 个 handler 都卡在
/// `Barrier::wait()`，到齐后一起放行、回 `{}`（`query` 的 JSONEachRow 能解析）。
/// 任何并发上限 < n（含旧语义的 `Semaphore::new(0)`）都凑不齐 n 个在飞的请求，
/// 调用会卡到外层保险丝 —— 由保险丝变成 FAILED，不会卡住整个测试二进制。
async fn spawn_barrier(n: usize) -> String {
    let barrier = Arc::new(tokio::sync::Barrier::new(n));
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let barrier = Arc::clone(&barrier);
        async move {
            barrier.wait().await;
            axum::response::Response::new(axum::body::Body::from("{}"))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// `max_concurrency` 走**配置**（`Some` 显式给值、`None` 走 `from_config` 的默认
/// 32）：直接改私有字段会让 `cfg.max_concurrency` 的装配路径失去覆盖 —— 那样
/// `from_config` 里删掉那行，本文件的并发测试照样绿。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> ClickhouseClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: ClickhouseConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    ClickhouseClient::from_config(cfg).unwrap()
}

/// `from_config` 把 `cfg.breaker` 真的接上了（checklist §1 验收②）。
///
/// 本 crate 没有 `config_wires_timeout_concurrency_and_breaker` 那条总用例（超时与
/// 并发由本文件其它用例经 `client_at` 覆盖），故只对 breaker 补这条最小版。
///
/// 用一份**非默认**的 breaker 配置：`failure_ratio: 1.1` 永不触发。连续 5 次失败后
/// 必须**仍是 Closed**。失败用本地 `call` 直接驱动 —— 熔断器对 `f` 的 `Err` 记为
/// 失败，与真实出站失败同一条记账路径
/// （`ecat-circuit-breaker/src/breaker.rs:182-190`），不依赖 mock 也不走网络。
/// 若 `from_config` 漏接 `cfg.breaker`（默认 0.5 会在第 5 次失败后开断）⇒ 必红。
#[tokio::test]
async fn config_wires_breaker() {
    let cfg: ClickhouseConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = ClickhouseClient::from_config(cfg).unwrap();
    for _ in 0..5 {
        let _ = c
            .breaker()
            .call(|| async { Err::<(), std::io::Error>(std::io::Error::other("boom")) })
            .await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "配置里的 failure_ratio 1.1 没生效 —— from_config 是否漏接了 cfg.breaker？"
    );
}

/// 超时真的开火（spec §8 判据 2）。
///
/// **必须用 1 秒超时 + 5 秒 mock**：`from_config` 建的 client 自带 reqwest 的
/// 30 秒总超时，mock 只拖 5 秒时它来不及开火；若我们的外层没接上，
/// 调用会**成功返回** ⇒ 断言失败。这条测试不是空验收。
#[tokio::test]
async fn query_times_out_with_timeout_error() {
    let url = spawn_slow_clickhouse(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let before = TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst);
    let err = ecat_data::SqlExecutor::query(&c, "SELECT 1")
        .await
        .expect_err("必须超时");
    assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
    assert!(
        TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst) > before,
        "应计入 Rdbms 维度"
    );
}

/// `TsdbClient` 路径独立计维度 —— 一个 client 两条路径，别串到一个槽里。
/// 判据是严格的：`Tsdb` 槽**恰好 +1**，且证人槽纹丝不动。
#[tokio::test]
async fn tsdb_path_counts_its_own_dimension() {
    let url = spawn_slow_clickhouse(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    // 证人槽必须是**本测试二进制内没有任何其它用例会写**的槽 —— `Rdbms` 不行
    // （另两条用例在写它，libtest 并行时它们的 +1 会插进本用例的观测窗口）。
    // 加用例前先 `grep -rn 'BackendKind::Storage'`；要写它就先换一个自由槽。
    let witness_before = TIMEOUTS[BackendKind::Storage as usize].load(Ordering::SeqCst);
    let tsdb_before = TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst);
    let err = ecat_data::TsdbClient::query(&c, "SELECT 1")
        .await
        .expect_err("必须超时");
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    // 严格 `== +1`（而非 `>`）成立的前提：`Tsdb` 槽在**本测试二进制内只有一个写者**。
    // metrics.rs 的用例已改成不推进它（见那里的说明）；谁要新增写 Tsdb 槽的用例，
    // 先想清楚这条断言会不会被它的 +1 掩护成绿。
    assert_eq!(
        TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst),
        tsdb_before + 1,
        "Tsdb 路径必须恰好计一次 Tsdb 槽"
    );
    assert_eq!(
        TIMEOUTS[BackendKind::Storage as usize].load(Ordering::SeqCst),
        witness_before,
        "Tsdb 的超时不得落到别的槽（证人槽约束见上）"
    );
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow_clickhouse(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::SqlExecutor::query(&c, "SELECT 1").await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(
        c.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::SqlExecutor::query(&c, "SELECT 1")
        .await
        .expect_err("熔断已打开");
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断打开后必须立即返回，实际 {:?}",
        start.elapsed()
    );
    assert!(matches!(err, RdbmsError::Connection(_)), "got: {err:?}");
}

/// **两条路径共用同一个 `Breaker`** —— 本批把 ClickHouse 当最难 case 的核心理由
/// （同一服务器、同一故障域）：`SqlExecutor` 打到 Open 后，`TsdbClient` 也必须被
/// 同一个熔断器拒绝。若有人拆成两个 `Breaker` 字段，这条会红。
///
/// 判据用**错误语义**（`Unavailable` + "circuit breaker is open"）而非墙钟 ——
/// 语义严格强于计时（Task 4 A/B 探针结论）；墙钟只作次判据。不共享时该调用会
/// 走满 1 秒超时并返回 `DeadlineExceeded` ⇒ 语义断言先红。
#[tokio::test]
async fn open_breaker_on_rdbms_path_also_rejects_tsdb_path() {
    let url = spawn_slow_clickhouse(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::SqlExecutor::query(&c, "SELECT 1").await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Open,
        "前置：Rdbms 路径须先打到 Open"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::TsdbClient::query(&c, "SELECT 1")
        .await
        .expect_err("熔断已打开，Tsdb 路径不得放行");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// **`_with` 落到 trait 默认实现，绝不触碰熔断器**（出入 7）。
///
/// 三个方法默认返回「不支持」—— 那是**调用方的用法错**，不是后端故障。
/// 若有人给 ClickHouse 补上 `_with` 的实现并"顺手"包进 `guarded`，
/// 5 次「不支持」就会把熔断器打开，之后**正常查询全被拒绝**。
#[tokio::test]
async fn unsupported_with_methods_do_not_trip_the_breaker() {
    let url = spawn_slow_clickhouse(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        let _ = ecat_data::SqlExecutor::execute_with(&c, "UPDATE t SET x = ?", &[]).await;
        let _ = ecat_data::SqlExecutor::query_with(&c, "SELECT ?", &[]).await;
        let _ = ecat_data::SqlExecutor::query_write(&c, "INSERT INTO t VALUES (?)", &[]).await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "「不支持」不是后端故障，不得计入熔断窗口"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

/// `transaction()` 是常量错误、不含 I/O —— 同样不得触碰熔断器（出入 6）。
#[tokio::test]
async fn transaction_error_does_not_trip_the_breaker() {
    let url = spawn_slow_clickhouse(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        assert!(ecat_data::RdbmsClient::transaction(&c).await.is_err());
    }
    assert_eq!(c.breaker().state(), BreakerState::Closed);
    assert_eq!(c.breaker().opened_total(), 0);
}

/// 并发上限真的封顶（spec §8 判据 5）。
/// 起 N+1 个并发，断言**同时在线**的请求数不超过 N。
/// 每个请求 sleep 50ms，远小于 30 秒超时 —— 排队的那个不会因超时而失败。
#[tokio::test]
async fn concurrency_cap_limits_in_flight_requests() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let url = spawn_slow_clickhouse(Duration::from_millis(50), Arc::clone(&in_flight)).await;
    let c = Arc::new(client_at(&url, 30, Some(2)));

    let mut handles = Vec::new();
    for _ in 0..3 {
        let c = Arc::clone(&c);
        let peak = Arc::clone(&peak);
        let in_flight = Arc::clone(&in_flight);
        handles.push(tokio::spawn(async move {
            // 采样：本请求在飞时看到的峰值
            let mut last = 0;
            let sampler = tokio::spawn(async move {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    last = last.max(in_flight.load(Ordering::SeqCst));
                }
                last
            });
            let _ = ecat_data::SqlExecutor::query(c.as_ref(), "SELECT 1").await;
            let seen = sampler.await.unwrap();
            peak.fetch_max(seen, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "并发上限是 2，实测峰值 {}",
        peak.load(Ordering::SeqCst)
    );
}

/// `max_concurrency: 0` = **不限并发**（不是 0 个许可）：5 个调用必须同时在飞、
/// 全部成功返回。旧语义（`Some(0) => Semaphore::new(0)`）在这里挂死 ——
/// 2 秒外层保险丝把它变成一条 FAILED；`Some(1)` / `Some(2)` 这类排队实现在
/// `Barrier` 上凑不齐 5 个、同样红。
///
/// **行为部分证明不了「不限」**：N=5 时 `None` 与 `Some(Semaphore::new(32))`
/// 不可区分（把 `Some(0) => None` 改成给 32 个许可，下面 5 个并发照样全绿）。
/// 钉住 rustdoc 那句承诺靠第一行结构断言。
#[tokio::test]
async fn zero_max_concurrency_means_unlimited() {
    let url = spawn_barrier(5).await;
    let c = Arc::new(client_at(&url, 30, Some(0)));
    assert!(
        c.semaphore.is_none(),
        "0 应表示不限并发（不建信号量），而不是一个有限上限"
    );

    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = Arc::clone(&c);
        handles.push(tokio::spawn(async move {
            ecat_data::SqlExecutor::query(c.as_ref(), "SELECT 1").await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .expect("5 个调用没能同时在飞（挂死或排队）—— `max_concurrency: 0` 应表示不限并发")
            .unwrap()
            .expect("mock 回 `{}`，应解析成功");
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 「先拖住再回」的 mock：`delay` 之后回 200 空体。
async fn spawn_slow(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    spawn_with(move |_| {
        let in_flight = Arc::clone(&in_flight);
        axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
            let in_flight = Arc::clone(&in_flight);
            async move {
                in_flight.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                axum::response::Response::new(axum::body::Body::from(""))
            }
        })
    })
    .await
}

/// 「第一次拖住、之后立刻回 200 + `{}`」的 mock：第二次调用必须**成功**。
async fn spawn_slow_once(delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    spawn_with(move |_| {
        let seen = Arc::clone(&seen);
        axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
            let seen = Arc::clone(&seen);
            async move {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(delay).await;
                }
                axum::response::Response::new(axum::body::Body::from("{}"))
            }
        })
    })
    .await
}

/// 「N 个请求必须**同时**到齐才放行」的 mock：并发上限 < n 时凑不齐 n 个在飞请求。
async fn spawn_barrier(n: usize) -> String {
    let barrier = Arc::new(tokio::sync::Barrier::new(n));
    spawn_with(move |_| {
        let barrier = Arc::clone(&barrier);
        axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
            let barrier = Arc::clone(&barrier);
            async move {
                barrier.wait().await;
                axum::response::Response::new(axum::body::Body::from("{}"))
            }
        })
    })
    .await
}

/// 一次调用必须在**外层 5 秒内**返回 `RdbmsError::Timeout`，且推进 Rdbms 维度。
///
/// 外层 5 秒把「漏包 `guarded`」从挂死变成一条 FAILED —— 否则整个测试二进制会卡住。
async fn assert_times_out<T, F>(label: &str, fut: F) -> RdbmsError
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, RdbmsError>>,
{
    let before = timeout_counter(BackendKind::Rdbms).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert!(
        matches!(err, RdbmsError::Timeout(_)),
        "{label}: got {err:?}"
    );
    assert!(
        timeout_counter(BackendKind::Rdbms).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Rdbms 维度"
    );
    err
}

/// `max_concurrency` 走**配置**装配路径（直接改私有字段会让 from_config 失去覆盖）。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> TrinoClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: TrinoConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    TrinoClient::from_config(cfg).unwrap()
}

/// `from_config` 把三个字段都真的接上了；breaker 那半条用**非默认**配置区分：
/// `failure_ratio: 1.1` 时 5 次失败仍 `Closed`，退回默认 0.5 则会 `Open`。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: TrinoConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":1,
            "max_concurrency":3,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = TrinoClient::from_config(cfg).unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(
        c.semaphore.as_ref().unwrap().available_permits(),
        3,
        "显式给非 0 的值必须真的建出对应许可数的信号量"
    );
    assert_eq!(c.breaker().state(), BreakerState::Closed);

    let fail = || async { Err::<(), &str>("backend down") };
    for _ in 0..5 {
        let _ = c.breaker().call(fail).await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "配置里的 failure_ratio: 1.1 没生效（退回了默认 0.5？）"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

/// `0` = 显式禁用（`None`），未配置 = 30 秒。
#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let cfg: TrinoConfig =
        serde_json::from_str(r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":0}"#)
            .unwrap();
    assert_eq!(TrinoClient::from_config(cfg).unwrap().query_timeout, None);
}

/// `max_concurrency: 0` = **不限并发**（不是 0 个许可）：5 个调用必须同时在飞。
/// 结构断言钉住 rustdoc 的承诺（N=5 时行为上区分不出 32 个许可的信号量）。
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
            ecat_data::SqlExecutor::query(c.as_ref(), "select * from t").await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .expect("5 个调用没能同时在飞（挂死或排队）—— `max_concurrency: 0` 应表示不限并发")
            .unwrap()
            .expect("mock 回 `{}`，应解析成空行集");
    }
}

/// 超时真的开火，且只落 Rdbms 槽。
#[tokio::test]
async fn execute_times_out_and_counts_rdbms_dimension() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let witness = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);
    assert_times_out(
        "execute",
        ecat_data::SqlExecutor::execute(&c, "create table t (i bigint)"),
    )
    .await;
    assert_eq!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst),
        witness,
        "Rdbms 的超时不得落到别的槽"
    );
}

/// **两条 I/O 路径各打一次**：漏包任意一个，它的调用会等满 5 秒 mock，
/// 外层保险丝报「内层超时没开火」。2 次 < 5 条失败窗口 ⇒ 熔断不会在途中打开。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    assert_times_out(
        "execute",
        ecat_data::SqlExecutor::execute(&c, "create table t (i bigint)"),
    )
    .await;
    assert_times_out(
        "query",
        ecat_data::SqlExecutor::query(&c, "select * from t"),
    )
    .await;
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "2 次调用打不满 5 条窗口"
    );
}

/// **整条轮询循环共用一个超时预算**：第一页立即回（带 nextUri），第二页拖 2.5 秒。
/// 1 秒预算若只包住第一个请求，本调用会在 ~2.5 秒后**成功返回**（断言先红时间）；
/// 包住整个循环则 1 秒判超时。
#[tokio::test]
async fn timeout_covers_the_whole_next_uri_polling_loop() {
    let url = spawn_with(|url| {
        let first = serde_json::json!({"id": "q1", "nextUri": format!("{url}/slow")}).to_string();
        axum::Router::new()
            .route(
                "/v1/statement",
                post(move || {
                    let first = first.clone();
                    async move { (StatusCode::OK, first) }
                }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(2500)).await;
                    (StatusCode::OK, serde_json::json!({"id": "q1"}).to_string())
                }),
            )
    })
    .await;
    let c = client_at(&url, 1, None);
    let start = std::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        ecat_data::SqlExecutor::execute(&c, "select 1"),
    )
    .await
    .expect("内层超时没开火（轮询循环没包在同一个 guarded 里？）")
    .unwrap_err();
    assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "超时必须覆盖 nextUri 分页；实际耗时 {:?}",
        start.elapsed()
    );
}

/// 熔断真的打开：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::SqlExecutor::execute(&c, "create table t (i bigint)").await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(
        c.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::SqlExecutor::execute(&c, "create table t (i bigint)")
        .await
        .expect_err("熔断已打开");
    assert!(matches!(err, RdbmsError::Connection(_)), "got: {err:?}");
    assert!(
        err.to_string().contains("circuit breaker is open"),
        "熔断拒绝的消息不对，got: {err}"
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// 并发上限真的封顶：3 个并发、同时在线 ≤ 2。
#[tokio::test]
async fn concurrency_cap_limits_in_flight_requests() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let url = spawn_slow(Duration::from_millis(50), Arc::clone(&in_flight)).await;
    let c = Arc::new(client_at(&url, 30, Some(2)));

    let mut handles = Vec::new();
    for _ in 0..3 {
        let c = Arc::clone(&c);
        let peak = Arc::clone(&peak);
        let in_flight = Arc::clone(&in_flight);
        handles.push(tokio::spawn(async move {
            let mut last = 0;
            let sampler = tokio::spawn(async move {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    last = last.max(in_flight.load(Ordering::SeqCst));
                }
                last
            });
            let _ = ecat_data::SqlExecutor::query(c.as_ref(), "select * from t").await;
            let seen = sampler.await.unwrap();
            peak.fetch_max(seen, Ordering::SeqCst);
        }));
    }
    // 许可泄漏时第 3 个 task 永远排在 `acquire()` 上（许可在超时层外面），
    // 外层保险丝把它变成一条 FAILED，而不是让整个测试二进制挂死。
    for (i, h) in handles.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(10), h)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个并发任务挂死（许可泄漏？）"))
            .unwrap();
    }
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "并发上限是 2，实测峰值 {}",
        peak.load(Ordering::SeqCst)
    );
}

/// **超时的那次调用必须归还许可**：不归还时第二发会永远排在 `acquire()` 上。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let url = spawn_slow_once(Duration::from_secs(5)).await;
    let c = client_at(&url, 1, Some(1));
    let first = ecat_data::SqlExecutor::execute(&c, "create table t (i bigint)")
        .await
        .expect_err("第一发必须超时");
    assert!(matches!(first, RdbmsError::Timeout(_)), "got: {first:?}");

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::SqlExecutor::execute(&c, "create table t (i bigint)"),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
    assert_eq!(second, 0, "`execute` 对无 stats 的 2xx 回 0");
}

/// `transaction()` 是常量错误、不含 I/O —— 不得触碰熔断器。
#[tokio::test]
async fn transaction_error_does_not_trip_the_breaker() {
    let url = spawn_slow(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        assert!(ecat_data::RdbmsClient::transaction(&c).await.is_err());
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "常量错误不是后端故障，不得计入熔断窗口"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 「先拖住再回」的 mock（模式 ①）：`delay` 之后回 200 空体。
async fn spawn_slow(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new().fallback(move |_req: axum::extract::Request| {
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

/// 「第一次拖住、之后立刻回 200 空体」的 mock（模式 ④）。
async fn spawn_slow_once(delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |_req: axum::extract::Request| {
        let seen = Arc::clone(&seen);
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(delay).await;
            }
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

/// 「N 个请求必须**同时**到齐才放行」的 mock。
async fn spawn_barrier(n: usize) -> String {
    let barrier = Arc::new(tokio::sync::Barrier::new(n));
    let app = axum::Router::new().fallback(move |_req: axum::extract::Request| {
        let barrier = Arc::clone(&barrier);
        async move {
            barrier.wait().await;
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

/// 一次调用必须在**外层 5 秒内**返回 `DeadlineExceeded`，且推进 Document 维度。
///
/// 外层 5 秒是**把挂死变成红灯**：漏包 `guarded` 的后果不是报错而是永远不返回。
async fn assert_times_out<T, F>(label: &str, fut: F) -> Error
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, Error>>,
{
    let before = timeout_counter(BackendKind::Document).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "{label}: {err}");
    // 宽列没有独立维度，`guarded` 用 Document ⇒ reason 必须是 "document"。
    assert_eq!(
        err.reason, "document",
        "{label}: 超时 reason 应是 kind.slug()"
    );
    assert!(
        timeout_counter(BackendKind::Document).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Document 维度"
    );
    err
}

/// `max_concurrency` 走**配置**：直接改私有字段会让装配路径失去覆盖。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> HbaseClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: HbaseConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    HbaseClient::from_config(cfg).unwrap()
}

/// breaker 那半条用**非默认**配置（`failure_ratio: 1.1`）：配置生效时 5 次失败
/// 仍 `Closed`，用了默认阈值（0.5）则会 `Open` ⇒ 能区分两者。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: HbaseConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":1,
            "max_concurrency":3,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = HbaseClient::from_config(cfg).unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(c.base_url, "http://127.0.0.1:1");
    assert!(c.username.is_none() && c.password.is_none());
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

#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let cfg: HbaseConfig =
        serde_json::from_str(r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":0}"#)
            .unwrap();
    assert_eq!(HbaseClient::from_config(cfg).unwrap().query_timeout, None);
}

/// 超时真的开火，且只落 Document 槽。证人槽 `Tsdb` 在本测试二进制里没有写者。
#[tokio::test]
async fn put_times_out_and_counts_document_dimension() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let witness = timeout_counter(BackendKind::Tsdb).load(Ordering::SeqCst);
    assert_times_out(
        "put",
        ecat_data::WideColumnClient::put(&c, "t", "r", &BTreeMap::new()),
    )
    .await;
    assert_eq!(
        timeout_counter(BackendKind::Tsdb).load(Ordering::SeqCst),
        witness,
        "Document 的超时不得落到别的槽"
    );
}

/// **四个 I/O 方法各打一次**：4 次调用 < 5 条失败窗口 ⇒ 熔断不会在途中打开，
/// 所以量的是「每个方法都套了 `guarded`」。漏包任意一个，它的调用会等满 5 秒。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    assert_times_out(
        "put",
        ecat_data::WideColumnClient::put(&c, "t", "r", &BTreeMap::new()),
    )
    .await;
    assert_times_out("get", ecat_data::WideColumnClient::get(&c, "t", "r")).await;
    assert_times_out("scan", ecat_data::WideColumnClient::scan(&c, "t", "p", 10)).await;
    assert_times_out("delete", ecat_data::WideColumnClient::delete(&c, "t", "r")).await;
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "4 次调用打不满 5 条窗口"
    );
}

#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::WideColumnClient::put(&c, "t", "r", &BTreeMap::new()).await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(
        c.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::WideColumnClient::put(&c, "t", "r", &BTreeMap::new())
        .await
        .expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert_eq!(
        err.reason, "hbase",
        "熔断错误的 reason 是产品名（超时路径才是 kind.slug()）"
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// 并发上限真的封顶：起 3 个并发、断言同时在线 ≤ 2。
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
            let _ = ecat_data::WideColumnClient::put(c.as_ref(), "t", "r", &BTreeMap::new()).await;
            let seen = sampler.await.unwrap();
            peak.fetch_max(seen, Ordering::SeqCst);
        }));
    }
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

/// `max_concurrency: 0` = **不限并发**（不是 0 个许可）：5 个调用必须同时在飞。
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
            ecat_data::WideColumnClient::put(c.as_ref(), "t", "r", &BTreeMap::new()).await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .expect("5 个调用没能同时在飞（挂死或排队）—— `max_concurrency: 0` 应表示不限并发")
            .unwrap()
            .expect("mock 回 200，put 只看状态码，应成功");
    }
}

/// **超时的那次调用必须归还许可**：第二发会永远排在 `acquire()` 上，只能被
/// 2 秒保险丝抓住。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let url = spawn_slow_once(Duration::from_secs(5)).await;
    let c = client_at(&url, 1, Some(1));
    let first = ecat_data::WideColumnClient::put(&c, "t", "r", &BTreeMap::new())
        .await
        .expect_err("第一发必须超时");
    assert_eq!(first.code, ErrorCode::DeadlineExceeded, "got: {first}");

    tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::WideColumnClient::put(&c, "t", "r", &BTreeMap::new()),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
}

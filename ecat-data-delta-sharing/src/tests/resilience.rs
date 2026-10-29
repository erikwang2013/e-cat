// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use axum::body::Body;
use axum::http::Request;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 拖 `delay` 后才回合法空列表。
async fn spawn_slow(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new().fallback(move |_req: Request<Body>| {
        let in_flight = Arc::clone(&in_flight);
        async move {
            in_flight.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            axum::response::Response::new(Body::from(r#"{"items":[]}"#))
        }
    });
    serve(app)
}

/// 第一次拖住、之后立刻回合法空列表。
async fn spawn_slow_once(delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |_req: Request<Body>| {
        let seen = Arc::clone(&seen);
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(delay).await;
            }
            axum::response::Response::new(Body::from(r#"{"items":[]}"#))
        }
    });
    serve(app)
}

/// N 个请求必须**同时**到齐才放行（确定性并发上限探针）。
async fn spawn_barrier(n: usize) -> String {
    let barrier = Arc::new(tokio::sync::Barrier::new(n));
    let app = axum::Router::new().fallback(move |_req: Request<Body>| {
        let barrier = Arc::clone(&barrier);
        async move {
            barrier.wait().await;
            axum::response::Response::new(Body::from(r#"{"items":[]}"#))
        }
    });
    serve(app)
}

fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> DeltaSharingClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    client_from(&format!(
        r#"{{"base_url": "{url}", "bearer_token": "t", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
}

/// `from_config` 真的接上了三个字段：非默认 breaker 阈值、超时、并发许可数。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: DeltaSharingConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","bearer_token":"t","query_timeout_secs":1,
            "max_concurrency":3,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = DeltaSharingClient::from_config(cfg).unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(c.semaphore.as_ref().unwrap().available_permits(), 3);

    let fail = || async { Err::<(), &str>("backend down") };
    for _ in 0..5 {
        let _ = c.breaker().call(fail).await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "failure_ratio: 1.1 没生效（退回默认 0.5 时 5 次失败会打开）"
    );
}

#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let c = client_at("http://127.0.0.1:1", 0, None);
    assert_eq!(c.query_timeout, None);
}

/// `max_concurrency: 0` = 不限并发（不建信号量），5 个请求必须同时在飞。
#[tokio::test]
async fn zero_max_concurrency_means_unlimited() {
    let url = spawn_barrier(5).await;
    let c = Arc::new(client_at(&url, 30, Some(0)));
    assert!(
        c.semaphore.is_none(),
        "0 应表示不限并发，而不是一个有限上限"
    );

    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = Arc::clone(&c);
        handles.push(tokio::spawn(
            async move { c.list_shares().await.map(|_| ()) },
        ));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .expect("5 个请求没能同时在飞（挂死或排队）")
            .unwrap()
            .expect("mock 返回合法空列表");
    }
}

/// 每个 I/O 方法都必须套着 `guarded`（独立 client ⇒ 6 个超时不会互相打开熔断器）。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let before = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);

    let mut handles = Vec::new();
    for which in 0..6 {
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let c = client_at(&url, 1, None);
            match which {
                0 => c.list_shares().await.map(|_| ()),
                1 => c.list_schemas("s").await.map(|_| ()),
                2 => c.list_tables("s", "sc").await.map(|_| ()),
                3 => c.table_metadata("s", "sc", "t").await.map(|_| ()),
                4 => c.table_version("s", "sc", "t").await.map(|_| ()),
                _ => c.query_table("s", "sc", "t", &json!({})).await.map(|_| ()),
            }
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        let err = tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个方法没超时（漏包 guarded？）"))
            .unwrap()
            .expect_err("慢 mock 必须让调用超时");
        assert_eq!(err.code, ErrorCode::DeadlineExceeded, "方法 {i}: {err}");
    }
    assert!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst) > before,
        "超时必须计入 Storage 维度"
    );
}

/// 连续超时后熔断器打开并快速失败。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = c.list_shares().await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(c.breaker().opened_total(), 1);

    let start = std::time::Instant::now();
    let err = c.list_shares().await.expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert!(
        err.message().contains("circuit breaker is open"),
        "got: {err}"
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// 超时那次调用必须归还并发许可。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let url = spawn_slow_once(Duration::from_secs(5)).await;
    let c = client_at(&url, 1, Some(1));
    let first = c.list_shares().await.expect_err("第一发必须超时");
    assert_eq!(first.code, ErrorCode::DeadlineExceeded);

    tokio::time::timeout(Duration::from_secs(2), c.list_shares())
        .await
        .expect("第二发被排在许可上（超时路径没归还许可？）")
        .expect("许可归还后第二次必须成功");
}

/// 并发上限真的封顶：3 个并发、上限 2，同时在线峰值 ≤ 2。
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
            let sampler = tokio::spawn(async move {
                let mut seen = 0;
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    seen = seen.max(in_flight.load(Ordering::SeqCst));
                }
                seen
            });
            let _ = c.list_shares().await;
            peak.fetch_max(sampler.await.unwrap(), Ordering::SeqCst);
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

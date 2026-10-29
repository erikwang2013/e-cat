// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use axum::Router;
use axum::extract::OriginalUri;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, StorageClient, timeout_counter};
use ecat_errors::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 「拖 `delay` 再回 200 `x`」：`get` 把 200 当数据直接返回，单跳即可测超时。
async fn spawn_slow(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    let app = Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let in_flight = Arc::clone(&in_flight);
        async move {
            in_flight.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            "x"
        }
    });
    let (listener, base) = bind().await;
    serve(listener, app);
    base
}

/// 第一次拖住、之后立刻回 200 `x`（许可归还用例）。
async fn spawn_slow_once(delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let seen = Arc::clone(&seen);
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(delay).await;
            }
            "x"
        }
    });
    let (listener, base) = bind().await;
    serve(listener, app);
    base
}

/// 「N 个请求同时到齐才放行」：任何并发上限 < n 都凑不齐，由外层保险丝抓。
async fn spawn_barrier(n: usize) -> String {
    let barrier = Arc::new(tokio::sync::Barrier::new(n));
    let app = Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let barrier = Arc::clone(&barrier);
        async move {
            barrier.wait().await;
            "x"
        }
    });
    let (listener, base) = bind().await;
    serve(listener, app);
    base
}

/// 每次 LISTSTATUS 都延迟 `delay`，返回「下一层目录」，第 `pages` 页返回文件。
/// 用来验证一次 `list` 只有一个预算（不是每页一个）。
async fn spawn_paged(pages: usize, delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback(move |OriginalUri(_uri): OriginalUri| {
        let seen = Arc::clone(&seen);
        async move {
            let i = seen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            let body = if i + 1 < pages {
                format!(
                    r#"{{"FileStatuses":{{"FileStatus":[
                        {{"pathSuffix":"d{}","type":"DIRECTORY"}}]}}}}"#,
                    i + 1
                )
            } else {
                r#"{"FileStatuses":{"FileStatus":[
                    {"pathSuffix":"f.txt","type":"FILE"}]}}"#
                    .to_string()
            };
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
        }
    });
    let (listener, base) = bind().await;
    serve(listener, app);
    base
}

fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> HadoopClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: HadoopConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    HadoopClient::from_config(cfg).unwrap()
}

async fn assert_times_out<T, F>(label: &str, fut: F) -> Error
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, Error>>,
{
    let before = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "{label}: {err}");
    assert_eq!(
        err.reason, "storage",
        "{label}: 超时 reason 应是 kind.slug()"
    );
    assert!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Storage 维度"
    );
    err
}

/// `from_config` 真的接上了 timeout / 并发 / breaker 三个字段。
/// breaker 用非默认 `failure_ratio: 1.1` 区分「接上了」与「退回默认 0.5」。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: HadoopConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":1,
            "max_concurrency":3,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = HadoopClient::from_config(cfg).unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(
        c.semaphore.as_ref().unwrap().available_permits(),
        3,
        "显式非 0 值必须建出对应许可数"
    );
    assert_eq!(c.breaker().state(), BreakerState::Closed);

    let fail = || async { Err::<(), &str>("backend down") };
    for _ in 0..5 {
        let _ = c.breaker().call(fail).await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "failure_ratio: 1.1 没生效（退回了默认 0.5？）"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let c = client_at("http://127.0.0.1:1", 0, None);
    assert_eq!(c.query_timeout, None);
}

/// 超时开火且只落 Storage 槽（证人槽 `Cache` 在本测试二进制无写者）。
#[tokio::test]
async fn get_times_out_and_counts_storage_dimension() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let witness = timeout_counter(BackendKind::Cache).load(Ordering::SeqCst);
    assert_times_out("get", StorageClient::get(&c, "bucket", "key")).await;
    assert_eq!(
        timeout_counter(BackendKind::Cache).load(Ordering::SeqCst),
        witness,
        "Storage 的超时不得落到别的槽"
    );
}

/// 四个 I/O 方法各打一次（4 次 < 5 条失败窗口 ⇒ 熔断不会在途中打开）。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    assert_times_out("put", StorageClient::put(&c, "bucket", "key", b"x")).await;
    assert_times_out("get", StorageClient::get(&c, "bucket", "key")).await;
    assert_times_out("delete", StorageClient::delete(&c, "bucket", "key")).await;
    assert_times_out("list", StorageClient::list(&c, "bucket", "p")).await;
    assert_eq!(c.breaker().state(), BreakerState::Closed, "4 次打不满窗口");
}

/// 一次 `list` 只有一个预算：三页各 600ms，预算 1 秒 ⇒ 第二页必须整体超时。
/// 若 `guarded` 被包进翻页循环（每页一个预算），三页各自成功、本用例红。
#[tokio::test]
async fn whole_call_budget_covers_every_page_in_list() {
    let url = spawn_paged(3, Duration::from_millis(600)).await;
    let c = client_at(&url, 1, None);
    assert_times_out("list(递归)", StorageClient::list(&c, "bucket", "")).await;
}

#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = StorageClient::get(&c, "bucket", "key").await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(c.breaker().opened_total(), 1, "超时必须真的打开过熔断器");

    let start = std::time::Instant::now();
    let err = StorageClient::get(&c, "bucket", "key")
        .await
        .expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert_eq!(err.reason, "hadoop", "熔断错误的 reason 是组件名");
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// 并发上限封顶：3 个并发、上限 2，采样峰值 ≤ 2。
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
                let mut last = 0;
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    last = last.max(in_flight.load(Ordering::SeqCst));
                }
                last
            });
            let _ = StorageClient::get(c.as_ref(), "bucket", "key").await;
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

/// `max_concurrency: 0` = 不限并发（不是 0 个许可）。
#[tokio::test]
async fn zero_max_concurrency_means_unlimited() {
    let url = spawn_barrier(5).await;
    let c = Arc::new(client_at(&url, 30, Some(0)));
    assert!(
        c.semaphore.is_none(),
        "0 应表示不限并发（不建信号量），而不是有限上限"
    );

    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = Arc::clone(&c);
        handles.push(tokio::spawn(async move {
            StorageClient::get(c.as_ref(), "bucket", "key").await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .expect("5 个调用没能同时在飞（挂死或排队）")
            .unwrap()
            .expect("mock 回 200 `x`，应成功");
    }
}

/// 超时的那次调用必须归还许可：第二发否则永远排在 `acquire()` 上。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let url = spawn_slow_once(Duration::from_secs(5)).await;
    let c = client_at(&url, 1, Some(1));
    let first = StorageClient::get(&c, "bucket", "key")
        .await
        .expect_err("第一发必须超时");
    assert_eq!(first.code, ErrorCode::DeadlineExceeded, "got: {first}");

    tokio::time::timeout(
        Duration::from_secs(2),
        StorageClient::get(&c, "bucket", "key"),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
}

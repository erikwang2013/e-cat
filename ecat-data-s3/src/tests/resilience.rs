// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use ecat_errors::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 「先拖住再回」的 mock（模式 ①）：`delay` 之后回 200 `{}`。
/// 既有的裸 `TcpListener` 测试只应答一次握手、拖不住时间，故另起一个。
///
/// 体回 `{}` 而不是空体：本 crate 的 `list` 每页都解析 XML
/// （`xml::parse_list_xml` 只认 `<Key>` / `<NextContinuationToken>`），
/// 空体解析成「0 个 key、无 token」且**不报错** —— 将来把某条用例改成
/// 「预算内跑完」时会得到静默的空结果而不是红灯（T0-F 的警示）。
async fn spawn_slow(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let in_flight = Arc::clone(&in_flight);
        async move {
            in_flight.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
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

/// 「第一次拖住、之后立刻回 200 + `{}`」的 mock（模式 ④）。
///
/// 本 crate 用 `put` 做模式 ④：它只看状态码，`{}` 是合法体 ——
/// 第二次调用必须真的**成功**（不只是不挂死）。`list` 不参与模式 ④（要解析 XML）。
async fn spawn_slow_once(delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let seen = Arc::clone(&seen);
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(delay).await;
            }
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

/// 「N 个请求必须**同时**到齐才放行」的 mock：`n` 个 handler 都卡在
/// `Barrier::wait()`，到齐后一起放行、回 200 `{}`。
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

/// 「每页 `delay` 之后回一页 `ListBucketResult`」的 mock：**前 `pages_with_token` 页**
/// 各带一个 `NextContinuationToken`，之后不再带（翻页终止）。
///
/// 体必须是 `parse_list_xml` 认得的 XML —— 空体在这里不够用（同 T0-F 的警示）。
/// 必须**最终停止发 token**：否则「每页一个预算」的坏实现会一直转下去，
/// 变成挂死而不是红灯（外层 5 秒保险丝只保证它最终 FAILED）。
async fn spawn_paged(pages_with_token: usize, delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let seen = Arc::clone(&seen);
        async move {
            let i = seen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            let body = if i < pages_with_token {
                format!(
                    "<ListBucketResult><Contents><Key>k{i}</Key></Contents>\
                     <IsTruncated>true</IsTruncated>\
                     <NextContinuationToken>t{i}</NextContinuationToken></ListBucketResult>"
                )
            } else {
                format!("<ListBucketResult><Contents><Key>k{i}</Key></Contents></ListBucketResult>")
            };
            axum::response::Response::new(axum::body::Body::from(body))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 一次调用必须在**外层 5 秒内**返回 `DeadlineExceeded`，且推进本维度。
///
/// 外层 5 秒是**把挂死变成红灯**：漏包 `guarded` 的后果不是报错而是永远不返回，
/// 没有这层的话整个测试二进制会卡住而不是 FAILED（`ecat-data-redis/src/tests.rs:345-364`）。
///
/// 泛型 `T`：本 crate 的被测方法返回值有 `()`（put/delete）与 `Vec<String>`（list），
/// 调用点直接透传；`T: Debug` 是 `unwrap_err()` 的要求。
///
/// 本 crate 的错误类型是 `ecat_errors::Error` 的**别名** `StorageError`（`src/lib.rs`），
/// 所以这里写 `Error` 与之一字不差（别名不产生新类型）。
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
    // `reason` 是 kind 的 slug（`ecat-data/src/timeout.rs:108-118`）：
    // `guarded` 里 kind 传错（如 Cache）在这里红，而不是等到告警面板。
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

/// `max_concurrency` 走**配置**（`Some` 显式给值、`None` 走 `from_config` 的默认 32）：
/// 直接改私有字段会让 `cfg.max_concurrency` 的装配路径失去覆盖 —— 那样
/// `from_config` 里删掉那行，并发用例照样绿。
///
/// 本 crate **只有 `from_config`** 一个构造器，且四个字段都必填（没有简写可用）。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> S3Client {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: S3Config = serde_json::from_str(&format!(
        r#"{{"endpoint": "{url}", "region": "us-east-1", "access_key": "a", "secret_key": "b",
            "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    S3Client::from_config(cfg).unwrap()
}

/// `from_config` 把三个新字段都真的接上了（checklist §1 验收②）。
///
/// breaker 那半条按 2026-10-08 裁决（`07b5b9a`）：只用 `state() == Closed` 是**空验收**
/// —— 把 `cfg.breaker` 接成 `BreakerConfig::default()` 照样绿。用一份**非默认**配置
/// （`failure_ratio: 1.1`，不可能触发）并推到 5 次失败，配置生效时仍 `Closed`，
/// 用了默认阈值（0.5）则会 `Open` ⇒ 这条能区分两者。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: S3Config = serde_json::from_str(
        r#"{"endpoint":"http://127.0.0.1:1","region":"us-east-1","access_key":"a","secret_key":"b",
            "query_timeout_secs":1,"max_concurrency":3,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = S3Client::from_config(cfg).unwrap();
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
        "配置里的 failure_ratio: 1.1 没生效（退回了默认 0.5？5 次失败本该打不开）"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

/// `0` = 显式禁用（`None`），未配置 = 30 秒（`ecat-data-redis/src/tests.rs:334-343`）。
#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let cfg: S3Config = serde_json::from_str(
        r#"{"endpoint":"http://127.0.0.1:1","region":"us-east-1","access_key":"a","secret_key":"b",
            "query_timeout_secs":0}"#,
    )
    .unwrap();
    assert_eq!(S3Client::from_config(cfg).unwrap().query_timeout, None);
}

/// 超时真的开火（spec §8 判据 2），且只落 Storage 槽。
///
/// **必须用 1 秒超时 + 5 秒 mock**：`from_config` 建的 client 自带 reqwest 的
/// 30 秒总超时，mock 只拖 5 秒时它来不及开火；若我们的外层没接上，
/// 调用会**成功返回** ⇒ 断言失败。这条测试不是空验收。
#[tokio::test]
async fn put_times_out_and_counts_storage_dimension() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    // 证人槽必须是**本测试二进制内没有任何其它用例会写**的槽 —— `Cache` 满足。
    // ⚠️ 本 crate 自己的 kind 就是 `Storage`（`guarded` 与 metrics 用例都写它），
    // 拿它当证人等于用「自己写自己」自证 ⇒ 单独跑也绿的空验收。
    // 谁要新增写 Cache 槽的用例，先换一个自由槽（见 T0 模板的槽分配规矩）。
    let witness = timeout_counter(BackendKind::Cache).load(Ordering::SeqCst);
    assert_times_out(
        "put",
        ecat_data::StorageClient::put(&c, "bucket", "key", b"data"),
    )
    .await;
    assert_eq!(
        timeout_counter(BackendKind::Cache).load(Ordering::SeqCst),
        witness,
        "Storage 的超时不得落到别的槽"
    );
}

/// **四个 I/O 方法各打一次**（多方法后端的必备用例）。
///
/// 4 次调用 **< 5 条失败窗口** ⇒ 熔断**不会**在途中打开，所以这条用例量的是
/// 「四个方法**各自**都套了 `guarded`」，而不是被前一条的熔断器挡下来的假绿：
/// 漏包任意一个，它的调用会老老实实等满 5 秒 mock —— 外层 5 秒保险丝报
/// 「内层超时没开火」。
///
/// ⚠️ **不许把这条扩到 5 个方法以上**：窗口被填满后尾部 `state() == Closed`
/// 会红，且失败信息会指向「漏包 guarded」—— 误导（实际是窗口满了）。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    assert_times_out(
        "put",
        ecat_data::StorageClient::put(&c, "bucket", "key", b"data"),
    )
    .await;
    assert_times_out("get", ecat_data::StorageClient::get(&c, "bucket", "key")).await;
    assert_times_out(
        "delete",
        ecat_data::StorageClient::delete(&c, "bucket", "key"),
    )
    .await;
    assert_times_out("list", ecat_data::StorageClient::list(&c, "bucket", "p")).await;
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "4 次调用打不满 5 条窗口"
    );
}

/// **一次 `list` 只有一个预算**（罩住整段翻页）：mock 每页 600ms、前 2 页各带一个
/// `NextContinuationToken`（第 3 页不再带，让翻页终止），预算 1 秒 ⇒ 第 2 页
/// （累计 1.2s）必须整体超时。把 `guarded` 包进翻页循环里（每页一个预算）时
/// 三页各自成功、`list` 返回 `Ok(..)` ⇒ **本用例红**（`unwrap_err` on `Ok`）。
#[tokio::test]
async fn whole_call_budget_covers_every_page_in_list() {
    let url = spawn_paged(2, Duration::from_millis(600)).await;
    let c = client_at(&url, 1, None);
    assert_times_out(
        "list(翻页)",
        ecat_data::StorageClient::list(&c, "bucket", "p"),
    )
    .await;
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::StorageClient::put(&c, "bucket", "key", b"data").await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(
        c.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::StorageClient::put(&c, "bucket", "key", b"data")
        .await
        .expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert_eq!(
        err.reason, "s3",
        "熔断错误的 reason 是产品名（超时路径才是 kind.slug()）"
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// 并发上限真的封顶（spec §8 判据 5）：起 3 个并发、断言同时在线 ≤ 2。
/// 每个请求 sleep 50ms，远小于 30 秒超时 —— 排队的那个不会因超时而失败。
///
/// 每个 `h.await` 外再套一层 10 秒保险丝（checklist §(d-补)）：许可一旦泄漏，
/// 第 3 个 task 会永远排在 `acquire()` 上，**没有任何预算能结束它** ——
/// 裸等会让整个测试二进制挂死（Task 7 实测，人工 kill）。
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
            let _ = ecat_data::StorageClient::put(c.as_ref(), "bucket", "key", b"data").await;
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
            ecat_data::StorageClient::put(c.as_ref(), "bucket", "key", b"data").await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .expect("5 个调用没能同时在飞（挂死或排队）—— `max_concurrency: 0` 应表示不限并发")
            .unwrap()
            .expect("mock 回 200 `{}`，put 只看状态码，应成功");
    }
}

/// **超时的那次调用必须归还许可**（checklist §5 模式 ④，并发受限的后端必做）。
///
/// 不归还的实现在这里红：第二发会永远排在 `acquire()` 上（超时层在许可**里面**，
/// 排队的请求根本走不到超时），只能被 2 秒保险丝抓住。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let url = spawn_slow_once(Duration::from_secs(5)).await;
    let c = client_at(&url, 1, Some(1));
    let first = ecat_data::StorageClient::put(&c, "bucket", "key", b"data")
        .await
        .expect_err("第一发必须超时");
    assert_eq!(first.code, ErrorCode::DeadlineExceeded, "got: {first}");

    tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::StorageClient::put(&c, "bucket", "key", b"data"),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 「先拖住再回」的 mock（模式 ①）：`delay` 之后回 200 空体。
/// 既有的 `spawn_mock_write` / `spawn_mock_query` 是「立即应答 + 记录请求」，
/// 拖不住时间，故另起一个。
async fn spawn_slow(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
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

/// 「第一次拖住、之后立刻回 200 + `{}`」的 mock（模式 ④）。
///
/// 本 crate 用 `write` 做模式 ④：它**只看状态码**、不解析响应体，所以第二次
/// 必须是彻底的成功。
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

/// 一次调用必须在**外层 5 秒内**返回 `DeadlineExceeded`，且推进本维度。
///
/// 外层 5 秒是**把挂死变成红灯**：漏包 `guarded` 的后果不是报错而是永远不返回，
/// 没有这层的话整个测试二进制会卡住而不是 FAILED（`ecat-data-redis/src/tests.rs:345-364`）。
///
/// 泛型 `T`：本 crate 的被测方法返回值有 `Value`（`query`）与 `()`（`write`），
/// 调用点直接透传。`T: Debug` 是 `unwrap_err()` 的要求。
async fn assert_times_out<T, F>(label: &str, fut: F) -> Error
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, Error>>,
{
    let before = timeout_counter(BackendKind::Tsdb).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "{label}: {err}");
    // `reason` 是 kind 的 slug（`ecat-data/src/timeout.rs:108-118`）：
    // `guarded` 里 kind 传错（如 Search）在这里红，而不是等到告警面板。
    assert_eq!(err.reason, "tsdb", "{label}: 超时 reason 应是 kind.slug()");
    assert!(
        timeout_counter(BackendKind::Tsdb).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Tsdb 维度"
    );
    err
}

/// 一个 `write` 能直接发送的最小点集。
fn one_point() -> [DataPoint; 1] {
    [DataPoint::new("cpu").with_field("v", FieldValue::Int(1))]
}

/// `max_concurrency` 走**配置**（`Some` 显式给值、`None` 走 `from_config` 的默认 32）：
/// 直接改私有字段会让 `cfg.max_concurrency` 的装配路径失去覆盖 —— 那样
/// `from_config` 里删掉那行，并发用例照样绿。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> InfluxClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: InfluxConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "org": "o", "bucket": "b", "token": "t",
            "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    InfluxClient::from_config(cfg).unwrap()
}

/// `from_config` 把三个新字段都真的接上了（checklist §1 验收②）。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: InfluxConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","org":"o","bucket":"b","token":"t",
            "query_timeout_secs":1,"max_concurrency":3,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = InfluxClient::from_config(cfg).unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(
        c.semaphore.as_ref().unwrap().available_permits(),
        3,
        "显式给非 0 的值必须真的建出对应许可数的信号量"
    );
    // 非默认 breaker（`failure_ratio: 1.1` = 永不触发）：连续 5 次失败后必须
    // **仍是 Closed**。失败用本地 `call` 直接驱动 —— 熔断器对 `f` 的 `Err` 记为
    // 失败，与真实出站失败同一条记账路径
    // （`ecat-circuit-breaker/src/breaker.rs:182-190`），不依赖 mock 也不走网络。
    // 若 `from_config` 漏接 `cfg.breaker`（默认 0.5 会在第 5 次失败后开断）⇒ 必红。
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

/// `0` = 显式禁用（`None`），未配置 = 30 秒（`ecat-data-redis/src/tests.rs:334-343`）。
#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let cfg: InfluxConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","org":"o","bucket":"b","token":"t",
            "query_timeout_secs":0}"#,
    )
    .unwrap();
    assert_eq!(InfluxClient::from_config(cfg).unwrap().query_timeout, None);
}

/// 超时真的开火（spec §8 判据 2），且只落 Tsdb 槽。
///
/// **必须用 1 秒超时 + 5 秒 mock**：`from_config` 建的 client 自带 reqwest 的
/// 30 秒总超时，mock 只拖 5 秒时它来不及开火；若我们的外层没接上，
/// 调用会**成功返回** ⇒ 断言失败。这条测试不是空验收。
#[tokio::test]
async fn write_times_out_and_counts_tsdb_dimension() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    // 证人槽必须是**本测试二进制内没有任何其它用例会写**的槽 —— `Storage` 满足
    // （本 crate 的代码只用 `BackendKind::Tsdb`；metrics 用例也只用 Tsdb）。
    // 谁要新增写 Storage 槽的用例，先换一个自由槽（见 T0 模板的槽分配规矩）。
    let witness = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);
    assert_times_out("write", ecat_data::TsdbClient::write(&c, &one_point())).await;
    assert_eq!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst),
        witness,
        "Tsdb 的超时不得落到别的槽"
    );
}

/// **两条 I/O 路径各打一次**（多方法后端的必备用例，单方法 crate 不存在它）。
///
/// 2 次调用 < 5 条失败窗口 ⇒ 熔断**不会**在途中打开，所以这条用例量的是
/// 「两个方法**各自**都套了 `guarded`」，而不是被前一条的熔断器挡下来的假绿：
/// 漏包任意一个（如 `query`），它的调用会老老实实等满 5 秒 mock ——
/// 外层 5 秒保险丝报「内层超时没开火」。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    assert_times_out("write", ecat_data::TsdbClient::write(&c, &one_point())).await;
    assert_times_out(
        "query",
        ecat_data::TsdbClient::query(&c, "from(bucket: \"b\")"),
    )
    .await;
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "2 次调用打不满 5 条窗口"
    );
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::TsdbClient::write(&c, &one_point()).await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(
        c.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::TsdbClient::write(&c, &one_point())
        .await
        .expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert_eq!(
        err.reason, "influxdb",
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
            let _ = ecat_data::TsdbClient::write(c.as_ref(), &one_point()).await;
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
            ecat_data::TsdbClient::write(c.as_ref(), &one_point()).await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .expect("5 个调用没能同时在飞（挂死或排队）—— `max_concurrency: 0` 应表示不限并发")
            .unwrap()
            .expect("mock 回 200 `{}`，write 只看状态码，应成功");
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
    let first = ecat_data::TsdbClient::write(&c, &one_point())
        .await
        .expect_err("第一发必须超时");
    assert_eq!(first.code, ErrorCode::DeadlineExceeded, "got: {first}");

    tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::TsdbClient::write(&c, &one_point()),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
}

/// **`delete` 落到 trait 默认实现（`ecat-data/src/tsdb.rs:55`），绝不触碰熔断器。**
///
/// 默认返回「不支持」—— 那是**调用方的用法错**，不是后端故障。若有人给 InfluxDB 补上
/// 这个方法并「顺手」包进 `guarded`，8 次「不支持」就会把熔断器打开，
/// 之后**正常写入/查询全被拒绝**（checklist §7 陷阱 2）。
#[tokio::test]
async fn delete_default_does_not_trip_the_breaker() {
    let url = spawn_slow(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        assert!(
            ecat_data::TsdbClient::delete(&c, "DELETE FROM cpu")
                .await
                .is_err()
        );
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "「不支持」不是后端故障，不得计入熔断窗口"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

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

/// 断言进程级 `TIMEOUTS` 槽的三条用例的串行锁。
///
/// `TIMEOUTS` 是**进程级静态量**，而 libtest 默认并行：`query_times_out_*` 与
/// `repeated_timeouts_*` 都在写 `Rdbms` 槽，会让 `tsdb_path_counts_*` 的
/// 「Tsdb 的超时不得落到 Rdbms 槽」这条**见证槽**断言随机红（实测并行 20 次 17 红，
/// `--test-threads=1` 30/30 绿 —— 产品行为没错，红的是见证槽）。
/// `ecat-data/src/timeout.rs` 的同类用例靠「换一个无人写的证人槽」回避；这里三条
/// 用例都在盯同一个槽，只能串行。
static SLOT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// 超时真的开火（spec §8 判据 2）。
///
/// **必须用 1 秒超时 + 5 秒 mock**：`from_config` 建的 client 自带 reqwest 的
/// 30 秒总超时，mock 只拖 5 秒时它来不及开火；若我们的外层没接上，
/// 调用会**成功返回** ⇒ 断言失败。这条测试不是空验收。
#[tokio::test]
async fn query_times_out_with_timeout_error() {
    let _serial = SLOT_LOCK.lock().await;
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
#[tokio::test]
async fn tsdb_path_counts_its_own_dimension() {
    let _serial = SLOT_LOCK.lock().await;
    let url = spawn_slow_clickhouse(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let rdbms_before = TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst);
    let tsdb_before = TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst);
    let err = ecat_data::TsdbClient::query(&c, "SELECT 1")
        .await
        .expect_err("必须超时");
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    assert!(TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst) > tsdb_before);
    assert_eq!(
        TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst),
        rdbms_before,
        "Tsdb 的超时不得落到 Rdbms 槽"
    );
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let _serial = SLOT_LOCK.lock().await;
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

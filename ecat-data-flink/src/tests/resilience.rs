// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// `max_concurrency` 走**配置**（`Some` 显式给值、`None` 走默认 32）：
/// 直接改私有字段会让 `from_config` 的装配路径失去覆盖。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> FlinkClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: FlinkConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    FlinkClient::from_config(cfg).unwrap()
}

/// 一次调用必须在**外层 5 秒内**返回 `RdbmsError::Timeout`，且推进 Rdbms 维度。
/// 外层 5 秒把「漏包 guarded」从挂死变成一条 FAILED。
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

/// `from_config` 把超时、并发、熔断、session_properties 都真的接上了。
/// breaker 用非默认阈值（`failure_ratio: 1.1`）区分「配置生效」与「退回默认 0.5」。
#[test]
fn config_wires_timeout_concurrency_breaker_and_properties() {
    let cfg: FlinkConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":1,
            "max_concurrency":3,"breaker":{"failure_ratio":1.1},
            "session_properties":{"execution.runtime-mode":"batch"}}"#,
    )
    .unwrap();
    let client = FlinkClient::from_config(cfg).unwrap();
    assert_eq!(client.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(
        client.semaphore.as_ref().unwrap().available_permits(),
        3,
        "显式给非 0 的值必须真的建出对应许可数的信号量"
    );
    assert_eq!(
        client
            .session_properties
            .get("execution.runtime-mode")
            .map(String::as_str),
        Some("batch")
    );
    assert_eq!(client.breaker().state(), BreakerState::Closed);
}

#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let cfg: FlinkConfig =
        serde_json::from_str(r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":0}"#)
            .unwrap();
    assert_eq!(FlinkClient::from_config(cfg).unwrap().query_timeout, None);
}

/// `max_concurrency: 0` = **不限并发**（不是 0 个许可）：建 session 后 5 个查询
/// 必须同时卡在 statements 的 Barrier(5) 上、一起放行。有限上限（含旧语义
/// `Semaphore::new(0)`）凑不齐 5 个，被 2 秒保险丝变成 FAILED。
///
/// 行为部分分不出 `None` 与 `Some(32)`（5 < 32），钉住「不限」靠结构断言。
#[tokio::test]
async fn zero_max_concurrency_means_unlimited() {
    let mock = Arc::new(Mock::default());
    *mock.barrier.lock().unwrap() = Some(Arc::new(tokio::sync::Barrier::new(5)));
    mock.barrier_skip.store(1, Ordering::SeqCst); // 第 0 次提交是预热
    let url = spawn(Arc::clone(&mock)).await;
    let client = Arc::new(client_at(&url, 30, Some(0)));
    assert!(
        client.semaphore.is_none(),
        "0 应表示不限并发（不建信号量），而不是一个有限上限"
    );

    client.query("select 0").await.unwrap();

    let mut handles = Vec::new();
    for i in 0..5 {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            client.query(&format!("select {i}")).await
        }));
    }
    for (i, handle) in handles.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个查询没能同时在飞（挂死或排队）"))
            .unwrap()
            .unwrap();
    }
}

/// 超时覆盖**整条流程**：open/submit 很快、状态永远 RUNNING 时，外层预算
/// 必须在 1 秒内掐断轮询，而不是让调用无限等。漏包 guarded 的实现在这里挂到
/// 5 秒保险丝。
#[tokio::test]
async fn query_timeout_covers_the_polling_phase() {
    let mock = Arc::new(Mock::default());
    mock.always_running.store(true, Ordering::SeqCst);
    let url = spawn(Arc::clone(&mock)).await;
    let client = client_at(&url, 1, None);
    assert_times_out("query", ecat_data::SqlExecutor::query(&client, "select 1")).await;
    assert!(
        mock.status_polls.load(Ordering::SeqCst) > 1,
        "应先轮到轮询阶段才超时"
    );
}

/// **两条 I/O 路径各打一次**（单方法 crate 不存在它）。
/// 2 次调用 < 5 条失败窗口 ⇒ 熔断不会在途中打开。
///
/// 这里 mock 卡在 open session（流程第一步），证明 `execute` / `query` 都套了外壳。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let mock = Arc::new(Mock::default());
    mock.open_delay_ms.store(5000, Ordering::SeqCst);
    let url = spawn(Arc::clone(&mock)).await;
    let client = client_at(&url, 1, None);
    assert_times_out(
        "execute",
        ecat_data::SqlExecutor::execute(&client, "create table t (i bigint)"),
    )
    .await;
    assert_times_out(
        "query",
        ecat_data::SqlExecutor::query(&client, "select * from t"),
    )
    .await;
    assert_eq!(
        client.breaker().state(),
        BreakerState::Closed,
        "2 次调用打不满 5 条窗口"
    );
}

/// 连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let mock = Arc::new(Mock::default());
    mock.open_delay_ms.store(5000, Ordering::SeqCst);
    let url = spawn(Arc::clone(&mock)).await;
    let client = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::SqlExecutor::execute(&client, "create table t (i bigint)").await;
    }
    assert_eq!(client.breaker().state(), BreakerState::Open);
    assert_eq!(
        client.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::SqlExecutor::execute(&client, "create table t (i bigint)")
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

/// 超时的那次调用必须归还许可：否则第二发永远排在 `acquire()` 上
/// （超时层在许可**里面**，排队的请求走不到超时），只能被 2 秒保险丝抓住。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let mock = Arc::new(Mock::default());
    mock.open_delay_once_ms.store(5000, Ordering::SeqCst);
    let url = spawn(Arc::clone(&mock)).await;
    let client = client_at(&url, 1, Some(1));
    let first = ecat_data::SqlExecutor::execute(&client, "create table t (i bigint)")
        .await
        .expect_err("第一发必须超时");
    assert!(matches!(first, RdbmsError::Timeout(_)), "got: {first:?}");

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::SqlExecutor::execute(&client, "create table t (i bigint)"),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
    assert_eq!(second, 0, "`execute` 对成功操作回 0");
}

/// 并发上限真的封顶：上限 2、起 4 个查询，mock 在 statements 里记同时在飞峰值。
/// 每次 statement 持 100ms，足够让 4 个查询都尝试进入临界区。
#[tokio::test]
async fn concurrency_cap_limits_in_flight_requests() {
    let mock = Arc::new(Mock::default());
    mock.statement_hold_ms.store(100, Ordering::SeqCst);
    let url = spawn(Arc::clone(&mock)).await;
    let client = Arc::new(client_at(&url, 30, Some(2)));
    // 预热 session：让并发阶段量的是 statements 而不是建 session。
    client.query("select 0").await.unwrap();

    let mut handles = Vec::new();
    for i in 0..4 {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            client.query(&format!("select {i}")).await
        }));
    }
    for (i, handle) in handles.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个并发查询挂死（许可泄漏？）"))
            .unwrap()
            .unwrap();
    }
    assert!(
        mock.peak_in_flight.load(Ordering::SeqCst) <= 2,
        "并发上限是 2，实测峰值 {}",
        mock.peak_in_flight.load(Ordering::SeqCst)
    );
}

/// `transaction()` 是常量错误、不含 I/O —— 同样不得触碰熔断器。
#[tokio::test]
async fn transaction_error_does_not_trip_the_breaker() {
    let mock = Arc::new(Mock::default());
    let url = spawn(Arc::clone(&mock)).await;
    let client = client_at(&url, 30, None);
    for _ in 0..8 {
        assert!(ecat_data::RdbmsClient::transaction(&client).await.is_err());
    }
    assert_eq!(
        client.breaker().state(),
        BreakerState::Closed,
        "常量错误不是后端故障，不得计入熔断窗口"
    );
    assert_eq!(client.breaker().opened_total(), 0);
}

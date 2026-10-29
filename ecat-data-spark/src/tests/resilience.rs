// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）与单会话语句串行化测试。
use super::mock::spawn;
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// `max_concurrency` 走**配置**（`Some` 显式给值、`None` 走 `from_config` 的默认
/// 32）：直接改私有字段会让 `cfg.max_concurrency` 的装配路径失去覆盖 —— 那样
/// `from_config` 里删掉那行，并发用例照样绿。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> SparkClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: SparkConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    SparkClient::from_config(cfg).unwrap()
}

/// 一次调用必须在**外层 5 秒内**返回 `RdbmsError::Timeout`，且推进 Rdbms 维度。
///
/// 外层 5 秒把「漏包 guarded（永远不返回）」变成红灯而不是挂死整个测试二进制。
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

/// `from_config` 把超时、并发上限与熔断配置都真的接上了。
///
/// breaker 那半条用一份**非默认**配置（`failure_ratio: 1.1`，不可能触发）并推到
/// 5 次失败：配置生效时仍 `Closed`，用了默认阈值（0.5）则会 `Open` —— 能区分两者。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: SparkConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":1,
            "max_concurrency":3,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let c = SparkClient::from_config(cfg).unwrap();
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

/// `0` = 显式禁用（`None`），未配置 = 30 秒。
#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let cfg: SparkConfig =
        serde_json::from_str(r#"{"base_url":"http://127.0.0.1:1","query_timeout_secs":0}"#)
            .unwrap();
    assert_eq!(SparkClient::from_config(cfg).unwrap().query_timeout, None);
}

/// `max_concurrency: 0` = **不限并发**（不是 0 个许可）：5 个调用都要能完成。
/// 旧语义（`Semaphore::new(0)`）会在 `permit().await` 上挂死，结构断言钉住
/// 「不建信号量」这句承诺（N=5 时 `None` 与 32 个许可不可区分）。
#[tokio::test]
async fn zero_max_concurrency_means_unlimited() {
    let mock = new();
    let url = mock::spawn(mock.clone()).await;
    mock.set_starting_polls(0);
    mock.set_pending_polls(0);
    let c = Arc::new(client_at(&url, 30, Some(0)));
    assert!(
        c.semaphore.is_none(),
        "0 应表示不限并发（不建信号量），而不是一个有限上限"
    );

    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = Arc::clone(&c);
        handles.push(tokio::spawn(async move {
            ecat_data::SqlExecutor::query(c.as_ref(), "select 1").await
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个调用挂死（许可泄漏或 0 许可？）"))
            .unwrap()
            .expect("并发调用必须全部成功");
    }
}

/// 在飞的那次 `guarded` 必须**持有一个许可**，返回后归还。
///
/// 区分「`from_config` 真把 `max_concurrency` 接到信号量」与「用了默认 32」：
/// 默认时 `try_acquire()` 会成功，本用例红。
#[tokio::test]
async fn guarded_takes_a_permit_while_in_flight() {
    let mock = new();
    let url = spawn(Arc::clone(&mock)).await;
    let c = Arc::new(client_at(&url, 30, Some(1)));
    let started = Arc::new(tokio::sync::Notify::new());

    let handle = {
        let c = Arc::clone(&c);
        let started = Arc::clone(&started);
        tokio::spawn(async move {
            c.guarded(async move {
                started.notify_one();
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok::<(), RdbmsError>(())
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("guarded future 没跑到持许可的代码");
    assert!(
        c.semaphore.as_ref().unwrap().try_acquire().is_err(),
        "在飞的 guarded 调用必须占住唯一许可"
    );

    handle.await.unwrap().unwrap();
    assert!(
        c.semaphore.as_ref().unwrap().try_acquire().is_ok(),
        "guarded 返回后必须归还许可"
    );
}

/// 超时真的开火（且只落 Rdbms 槽）。
///
/// 必须用 1 秒超时 + 5 秒 mock：`from_config` 的 reqwest 自带 30 秒总超时，
/// mock 只拖 5 秒时它来不及开火；外层没接上则调用会**成功返回** ⇒ 断言失败。
#[tokio::test]
async fn execute_times_out_and_counts_rdbms_dimension() {
    let mock = new();
    let url = spawn(Arc::clone(&mock)).await;
    mock.slow(5_000, u64::MAX);
    let c = client_at(&url, 1, None);
    // 证人槽必须是本测试二进制内没有任何其它用例会写的槽。
    let witness = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);
    assert_times_out(
        "execute",
        ecat_data::SqlExecutor::execute(&c, "create table t (i int)"),
    )
    .await;
    assert_eq!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst),
        witness,
        "Rdbms 的超时不得落到别的槽"
    );
}

/// 两条 I/O 路径各打一次（多方法后端的必备用例）。
///
/// 2 次调用 < 5 条失败窗口 ⇒ 熔断不会在途中打开，所以这条量的是「两个方法
/// **各自**都套了 `guarded`」；漏包任意一个，它的调用会等满 5 秒 mock，
/// 被外层保险丝抓成「内层超时没开火」。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let mock = new();
    let url = spawn(Arc::clone(&mock)).await;
    mock.slow(5_000, u64::MAX);
    let c = client_at(&url, 1, None);
    assert_times_out(
        "execute",
        ecat_data::SqlExecutor::execute(&c, "create table t (i int)"),
    )
    .await;
    assert_times_out("query", ecat_data::SqlExecutor::query(&c, "select 1")).await;
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "2 次调用打不满 5 条窗口"
    );
}

/// 熔断真的打开：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let mock = new();
    let url = spawn(Arc::clone(&mock)).await;
    mock.slow(5_000, u64::MAX);
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::SqlExecutor::execute(&c, "create table t (i int)").await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(
        c.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::SqlExecutor::execute(&c, "create table t (i int)")
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

/// **超时的那次调用必须归还许可**：不归还时第二发永远排在 `acquire()` 上，
/// 只能被 2 秒保险丝抓住。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let mock = new();
    let url = spawn(Arc::clone(&mock)).await;
    mock.slow(5_000, 1); // 只有第一次请求慢
    let c = client_at(&url, 1, Some(1));
    let first = ecat_data::SqlExecutor::execute(&c, "create table t (i int)")
        .await
        .expect_err("第一发必须超时");
    assert!(matches!(first, RdbmsError::Timeout(_)), "got: {first:?}");

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::SqlExecutor::execute(&c, "create table t (i int)"),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
    assert_eq!(second, 0);
}

/// `transaction()` 是常量错误、不含 I/O —— 同样不得触碰熔断器。
#[tokio::test]
async fn transaction_error_does_not_trip_the_breaker() {
    let mock = new();
    let url = spawn(Arc::clone(&mock)).await;
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

/// **同一个 client 上的 statement 必须串行**：B 的 `POST /statements` 不得落在
/// A 的「提交 → 轮询出结果」之间。
///
/// mock 的每个响应延迟 30 ms，A 的语句要经过 5 次 `running` 轮询才变 available
/// （≈180 ms 窗口）。测试等到 A 的提交真的到达服务端后再启动 B：没有 session
/// 互斥时，B 会在 A 轮询期间就提交（events 顺序可见、`peak_active_statements`
/// 会是 2）；有互斥时 B 只可能在 `available 1` 之后提交。
#[tokio::test]
async fn concurrent_queries_serialize_statements_on_one_session() {
    let mock = new();
    mock.set_pending_polls(5);
    let url = spawn(Arc::clone(&mock)).await;
    mock.slow(30, u64::MAX);
    let c = Arc::new(client_at(&url, 30, None));

    let first = {
        let c = Arc::clone(&c);
        tokio::spawn(async move { ecat_data::SqlExecutor::query(c.as_ref(), "select 'a'").await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while mock.statements_submitted() == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("第一条语句一直没提交到服务端");

    let second = {
        let c = Arc::clone(&c);
        tokio::spawn(async move { ecat_data::SqlExecutor::query(c.as_ref(), "select 'b'").await })
    };
    let (a, b) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(first, second)
    })
    .await
    .expect("两个查询互相等待（死锁？）");
    a.unwrap().unwrap();
    b.unwrap().unwrap();

    assert_eq!(
        mock.peak_active_statements(),
        1,
        "同一 session 上同时有两条在飞语句"
    );
    let events = mock.events();
    let submitted_b = events
        .iter()
        .position(|e| e.starts_with("submit 2"))
        .unwrap_or_else(|| panic!("第二条语句没提交：{events:?}"));
    let finished_a = events
        .iter()
        .position(|e| e.starts_with("available 1"))
        .unwrap_or_else(|| panic!("第一条语句没完成：{events:?}"));
    assert!(
        submitted_b > finished_a,
        "B 在 A 完成前就提交了（语句交错）：{events:?}"
    );
    assert_eq!(mock.statement_sessions(), vec![1, 1]);
}

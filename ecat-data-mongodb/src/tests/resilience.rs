// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 池配置）测试。
//!
//! **没有模式 ①/④**：MongoDB 的线协议没有进程内 mock，真实方法打不到「慢后端」。
//! 所以这里用模式 ③（`std::future::pending()`）直接盯 `guarded` 外壳；
//! 「四个方法是否都包了」由 `all_four_methods_go_through_the_shell` 行为判据守着
//! （`grep -c 'self.guarded(' == 4` 只是补充：Task 11 探针 (h) 摘掉 `delete` 的壳，
//! 14 条全绿、只有 grep 从 4 变 3 —— 纯文本判据不够）。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// 指向一个**不会真的连接**的 URI：`ClientOptions::parse` 是纯本地解析，
/// 所以这个 client 建得起来，只是任何真实命令都会失败。
async fn client_at(timeout_secs: u64, max_pool_size: Option<u32>) -> MongoClient {
    let mp = match max_pool_size {
        Some(n) => format!(r#", "max_pool_size": {n}"#),
        None => String::new(),
    };
    let cfg: MongoConfig = serde_json::from_str(&format!(
        r#"{{"url": "mongodb://127.0.0.1:27017", "database": "app",
            "query_timeout_secs": {timeout_secs}{mp}}}"#
    ))
    .unwrap();
    MongoClient::from_config(cfg).await.unwrap()
}

/// 池大小真的从配置接到了驱动旋钮上（spec §8 对 MongoDB 的判据）。
/// 删掉 `build_options` 里那两行赋值 ⇒ 本用例红（`.is_some()` 落空）。
///
/// **`failure_ratio: 1.1` 那半段是裁决 07b5b9a（2026-10-08）**：`Closed` 是**默认**
/// 状态，只断言它等于没断言 —— `from_config` 漏接 `cfg.breaker`、装配成
/// `BreakerConfig::default()` 时照样绿（实测复现过的空验收形态）。给一个**不可能触发**
/// 的阈值，再本地驱动 5 次失败：真接上了就仍 `Closed`、`opened_total() == 0`；
/// 漏接成默认阈值（0.5）就会被这 5 次打满窗口翻成 `Open` ⇒ 红。
#[tokio::test]
async fn config_wires_timeout_pool_and_breaker() {
    let cfg: MongoConfig = serde_json::from_str(
        r#"{"url":"mongodb://127.0.0.1:27017","database":"app","query_timeout_secs":1,
            "max_pool_size":7,"min_pool_size":2,"breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let options = MongoClient::build_options(&cfg).await.unwrap();
    assert_eq!(
        options.max_pool_size,
        Some(7),
        "池上限必须来自配置（省略 = 驱动默认）"
    );
    assert_eq!(options.min_pool_size, Some(2));

    let c = MongoClient::from_config(cfg).await.unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(c.breaker().state(), BreakerState::Closed);

    // 5 次本地驱动的失败（各 1 秒超时，共约 5 秒）。窗口下限是 5 条失败。
    for _ in 0..5 {
        let _ = c.guarded(std::future::pending::<Result<(), Error>>()).await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "配置里的 failure_ratio 1.1 没生效 —— from_config 是否漏接了 cfg.breaker？"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

/// 省略池字段 = `None` = **不覆盖**，交给驱动默认（mongodb 3.8.0 实测 10）。
/// 谁「顺手」填一个默认数字（哪怕填 10），这条就把「默认值来源」这点改变了。
#[tokio::test]
async fn omitted_pool_size_leaves_the_driver_default() {
    let cfg: MongoConfig =
        serde_json::from_str(r#"{"url":"mongodb://127.0.0.1:27017","database":"app"}"#).unwrap();
    let options = MongoClient::build_options(&cfg).await.unwrap();
    assert_eq!(options.max_pool_size, None, "省略时不写死数字");
    assert_eq!(options.min_pool_size, None);
}

/// `0` = 显式禁用（`None`），未配置 = 30 秒。
#[tokio::test]
async fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    assert_eq!(client_at(0, None).await.query_timeout, None);
}

/// 模式 ③：`future::pending()` 永不就绪 —— 内层超时没接上就会挂死，被 5 秒保险丝抓住。
#[tokio::test]
async fn guarded_times_out_with_deadline_exceeded() {
    let c = client_at(1, None).await;
    let witness = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);
    let before = timeout_counter(BackendKind::Document).load(Ordering::SeqCst);
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        c.guarded(std::future::pending::<Result<serde_json::Value, Error>>()),
    )
    .await
    .unwrap_or_else(|_| panic!("内层超时没开火（漏包 guarded？）"))
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    assert_eq!(err.reason, "document", "超时 reason 应是 kind.slug()");
    assert!(timeout_counter(BackendKind::Document).load(Ordering::SeqCst) > before);
    assert_eq!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst),
        witness,
        "Document 的超时不得落到别的槽"
    );
}

/// 5 次超时 ⇒ 熔断打开、快速失败（每发 1 秒，共约 5 秒）。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let c = client_at(1, None).await;
    for _ in 0..5 {
        let _ = c.guarded(std::future::pending::<Result<(), Error>>()).await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(c.breaker().opened_total(), 1);

    let start = std::time::Instant::now();
    let err = c
        .guarded(std::future::pending::<Result<(), Error>>())
        .await
        .expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert_eq!(err.reason, "mongodb");
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回"
    );
}

/// **四个方法真的都走外壳**（行为判据，补 `grep -c 'self.guarded(' == 4` 的不足）。
///
/// 先把熔断器推到 `Open`（5 次本地驱动的失败），再调四个**真实**方法：
/// 走外壳的话 `breaker.call` 在**不碰网络**的前提下立刻拒绝 ⇒ `Unavailable` +
/// `"circuit breaker is open"`；某个方法漏包时它会直接落到驱动上，对着一个空地址
/// （实测 27017 无监听）报**别的**错（选主失败 30 秒后 `Internal: mongodb delete: …`），
/// 错误码与 message 都对不上 ⇒ 红。
///
/// 反向探针：把任一方法的 `guarded` 摘掉 ⇒ 本用例必红（Task 11 的探针 (h) 当时
/// 14 条全绿，就是缺这条）。**故意不给本用例单配短选主超时**：正常路径不碰网络
/// （< 1 秒），拉长只发生在「已经坏了」的探针跑里，不值得多一条 client 构造路径。
#[tokio::test]
async fn all_four_methods_go_through_the_shell() {
    fn refused(label: &str, r: Result<(), Error>) {
        let e = r.expect_err(&format!("熔断已打开，{label} 却成功了 —— 它漏包 guarded？"));
        assert_eq!(e.code, ErrorCode::Unavailable, "{label}: {e}");
        assert_eq!(e.message, "circuit breaker is open", "{label}: {e}");
    }

    let c = client_at(1, None).await;
    for _ in 0..5 {
        let _ = c.guarded(std::future::pending::<Result<(), Error>>()).await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);

    let doc = serde_json::json!({"a": 1});
    let empty = serde_json::json!({});
    let set = serde_json::json!({"$set": {"a": 2}});
    refused("insert", c.insert("col", &doc).await.map(|_| ()));
    refused("find", c.find("col", &empty).await.map(|_| ()));
    refused("update", c.update("col", &empty, &set).await.map(|_| ()));
    refused("delete", c.delete("col", &empty).await.map(|_| ()));
}

/// bson 转换失败**不是后端故障**，不得计入熔断窗口
/// （否则 5 次传错参就把正常写入也熔断了 —— 这正是「本地分支留在外面」的理由）。
#[tokio::test]
async fn bson_conversion_error_does_not_trip_the_breaker() {
    let c = client_at(1, None).await;
    for _ in 0..8 {
        let err = c
            .insert("col", &Value::Null)
            .await
            .expect_err("null 不是文档");
        assert!(err.to_string().contains("mongodb bson:"), "got: {err}");
    }
    assert_eq!(c.breaker().state(), BreakerState::Closed);
    assert_eq!(c.breaker().opened_total(), 0);
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 不连库的单元测试：`MssqlConfig` 解析、`Bind` 分派、客户端的池构造。
//!
//! 独立成文件是因为各源文件有 500 行上限（与 `ecat-data-sqlx/src/tests.rs`
//! 同一个理由）；模块本身仍是 `#[cfg(test)] mod tests`，与内联无异。
//!
//! 真库用例是另一个文件的事（env 门控），这里**只碰网络里必然失败的那部分**：
//! 建池、参数分派、以及「连不上」的错误映射。

use crate::MssqlClient;
use crate::MssqlConfig;
use crate::bind::Bind;
use ecat_data::{Dialect, RdbmsError, SqlExecutor};
use serde_json::{Value, json};
use std::time::Duration;
use tiberius::{ColumnData, ToSql};

#[test]
fn url_form_parses_into_fields() {
    let c = MssqlConfig::from_str("mssql://sa:pw@db.local:1433/app").unwrap();
    assert_eq!(c.host, "db.local");
    assert_eq!(c.port, 1433);
    assert_eq!(c.database.as_deref(), Some("app"));
    assert_eq!(c.username.as_deref(), Some("sa"));
    assert_eq!(c.password.as_deref(), Some("pw"));
}

/// ADO 串是 MSSQL 的惯用形态 —— 交给 tiberius 的解析器，不自己写。
#[test]
fn ado_string_is_accepted() {
    let c =
        MssqlConfig::from_str("Server=db.local,1433;Database=app;User Id=sa;Password=pw").unwrap();
    assert_eq!(c.host, "db.local");
    assert_eq!(c.database.as_deref(), Some("app"));
}

/// `skip_verify` 与 `ca_cert` 互斥 —— 与 `TlsClientConfig` 同一条规则：
/// 同时配置等于「既要校验又不校验」，必须报错而非静默选一个。
#[test]
fn skip_verify_and_ca_cert_are_mutually_exclusive() {
    let c: MssqlConfig = serde_json::from_str(
        r#"{"url": "mssql://h/db", "tls": {"skip_verify": true, "ca_cert": "/tmp/ca.pem"}}"#,
    )
    .unwrap();
    assert!(c.build_config().is_err());
}

#[test]
fn pool_defaults_match_documented_values() {
    let c: MssqlConfig = serde_json::from_str(r#"{"url": "mssql://h/db"}"#).unwrap();
    let p = c.pool();
    assert_eq!(p.max_connections, 10);
    assert_eq!(p.acquire_timeout, Duration::from_secs(30));
    assert_eq!(p.query_timeout, Some(Duration::from_secs(30)));
}

/// `query_timeout_secs: 0` = 禁用（与 `SqlxConfig` 同约定）。
#[test]
fn zero_query_timeout_means_disabled() {
    let c: MssqlConfig =
        serde_json::from_str(r#"{"url": "mssql://h/db", "query_timeout_secs": 0}"#).unwrap();
    assert_eq!(c.query_timeout(), None);
}

/// 默认会话初始化是 `SET ARITHABORT ON`。
/// 理由：ARITHABORT 取值不同会让同一查询在 SQL Server 上产生**多份执行计划**，
/// 造成计划缓存污染。显式空数组表示「主动关闭」。
#[test]
fn session_init_defaults_to_arithabort_on() {
    let c: MssqlConfig = serde_json::from_str(r#"{"url": "mssql://h/db"}"#).unwrap();
    assert_eq!(
        c.effective_session_init(),
        vec!["SET ARITHABORT ON".to_string()]
    );

    let off: MssqlConfig =
        serde_json::from_str(r#"{"url": "mssql://h/db", "session_init": []}"#).unwrap();
    assert!(
        off.effective_session_init().is_empty(),
        "显式空数组应覆盖默认"
    );
}

/// `build_config` 必须自己解析 `url`：serde 构造（没走 `from_str`）时
/// 也得拿到连接串里的主机与端口，两条构造路径不能分叉。
#[test]
fn build_config_parses_url_without_from_str() {
    let c: MssqlConfig =
        serde_json::from_str(r#"{"url": "mssql://sa:pw@db.local:1444/app"}"#).unwrap();
    let cfg = c.build_config().unwrap();
    assert_eq!(cfg.get_addr(), "db.local:1444");
}

/// URL 里的查询串不支持。`?encrypt=` 这类开关直接决定加密与证书校验，
/// 静默忽略等于「配了却没生效」—— 报错，并指向 `tls` 字段。
#[test]
fn url_query_string_is_rejected() {
    let err = MssqlConfig::from_str("mssql://h/db?encrypt=false").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("tls"), "错误信息要指向正确做法，got: {msg}");
}

/// 半配置的客户端证书（只有 cert 没有 key）必须报错，不能静默只配一半。
#[test]
fn half_client_certificate_is_rejected() {
    let c: MssqlConfig = serde_json::from_str(
        r#"{"url": "mssql://h/db", "tls": {"client_cert": "/tmp/client.pem"}}"#,
    )
    .unwrap();
    assert!(c.build_config().is_err());
}

/// `min > max` 必须夹到 max：否则 `warm_up()` 会一直取不满，阻塞到
/// acquire_timeout 才报错（与 `SqlxConfig` 同一条规则）。
#[test]
fn min_connections_is_clamped_to_max() {
    let c: MssqlConfig = serde_json::from_str(
        r#"{"url": "mssql://h/db", "max_connections": 2, "min_connections": 5}"#,
    )
    .unwrap();
    let p = c.pool();
    assert_eq!(p.max_connections, 2);
    assert_eq!(p.min_connections, 2);
}

/// 省略端口 → 默认 1433。
#[test]
fn url_without_port_uses_default_port() {
    let c = MssqlConfig::from_str("mssql://db.local/app").unwrap();
    assert_eq!(c.port, 1433);
    assert_eq!(c.database.as_deref(), Some("app"));
    assert_eq!(c.build_config().unwrap().get_addr(), "db.local:1433");
}

/// 没有 `@` → 无认证：既不凭空造用户名，也不因此报「半配置」错。
#[test]
fn url_without_credentials_has_no_auth() {
    let c = MssqlConfig::from_str("mssql://db.local/app").unwrap();
    assert_eq!(c.username, None);
    assert_eq!(c.password, None);
    assert!(c.build_config().is_ok());
}

/// 只给用户名不给密码 = 半配置 → 报错（与「半配置客户端证书」同一条规则）。
#[test]
fn username_without_password_is_rejected() {
    let c = MssqlConfig::from_str("mssql://sa@db.local/app").unwrap();
    assert_eq!(c.username.as_deref(), Some("sa"));
    assert_eq!(c.password, None);
    let msg = c.build_config().unwrap_err().to_string();
    assert!(msg.contains("成对"), "got: {msg}");
}

/// IPv6 字面量：**方括号保留**。`get_addr()` 拼的是 `host:port` 交给
/// `TcpStream::connect`，方括号正是 std 认的 IPv6 形式（去掉反而连不上）。
/// 带端口与不带端口两种都要拆对 —— 不带端口时 `[::1]` 里的冒号不能用当分隔符。
#[test]
fn ipv6_literal_host_keeps_brackets() {
    let c = MssqlConfig::from_str("mssql://[::1]:1433/app").unwrap();
    assert_eq!(c.host, "[::1]");
    assert_eq!(c.port, 1433);
    assert_eq!(c.build_config().unwrap().get_addr(), "[::1]:1433");

    let c = MssqlConfig::from_str("mssql://[::1]/app").unwrap();
    assert_eq!(c.host, "[::1]");
    assert_eq!(c.port, 1433);
    assert_eq!(c.build_config().unwrap().get_addr(), "[::1]:1433");
}

/// `Bind` 按 JSON 形态分派：字符串/整数/浮点/布尔/NULL 各归各的变体。
///
/// 整数**优先** `i64`：`serde_json` 的 `Number` 同时装得下 `i64` 与 `u64`，
/// 顺序写反了会把 `7` 变成 `7.0`（与 sqlx 路径的 `as_i64` 先行一致）。
#[test]
fn bind_dispatches_each_json_shape() {
    assert_eq!(Bind::from_json(&json!("s")), Bind::Str("s".into()));
    assert_eq!(Bind::from_json(&json!(7)), Bind::I64(7));
    assert_eq!(Bind::from_json(&json!(-7)), Bind::I64(-7));
    assert_eq!(Bind::from_json(&json!(1.5)), Bind::F64(1.5));
    assert_eq!(Bind::from_json(&json!(true)), Bind::Bool(true));
    assert_eq!(Bind::from_json(&Value::Null), Bind::Null);
}

/// `u64` 超出 `i64::MAX` → 走 `f64`（有精度损失，与 sqlx 路径同一取舍）。
/// 钉住它是因为「整数一律 i64」的直觉在这里会溢出。
#[test]
fn bind_huge_unsigned_number_falls_back_to_f64() {
    assert!(matches!(Bind::from_json(&json!(u64::MAX)), Bind::F64(_)));
}

/// 数组/对象没有 SQL 标量对应 → JSON 文本（sqlx 路径的 `_` 分支同款）。
#[test]
fn bind_array_and_object_become_json_text() {
    assert_eq!(Bind::from_json(&json!([1, 2])), Bind::Str("[1,2]".into()));
    assert_eq!(
        Bind::from_json(&json!({"a": 1})),
        Bind::Str(r#"{"a":1}"#.into())
    );
}

/// NULL 的表示：JSON null 不带类型，而 TDS 参数是强类型的，必须挑一个 ——
/// 挑的是 `nvarchar` 的 NULL（`ColumnData::String(None)`），与 sqlx 路径的
/// `q.bind(None::<String>)` 同一选择，两者在服务端看到的是带类型的 NULL。
///
/// 钉住它是因为这个选择**没有编译期约束**：换成 `ColumnData::I32(None)` 也能编过，
/// 但 NULL 的隐式转换目标就变了。
#[test]
fn bind_null_is_a_typed_null_not_a_missing_parameter() {
    match Bind::Null.to_sql() {
        ColumnData::String(None) => {}
        other => panic!("NULL 应为 nvarchar 的 NULL，got: {other:?}"),
    }
    // 有值的几个都带上自己的值，不能被上面那条的空类型顶掉。
    assert!(matches!(Bind::I64(3).to_sql(), ColumnData::I64(Some(3))));
    assert!(matches!(
        Bind::Bool(true).to_sql(),
        ColumnData::Bit(Some(true))
    ));
    assert!(matches!(
        Bind::Str("x".into()).to_sql(),
        ColumnData::String(Some(_))
    ));
}

/// 先占端口再释放 —— 得到一个必然没人监听的地址（不依赖外部环境）。
fn dead_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn client_config(extra: &str) -> MssqlConfig {
    serde_json::from_str(&format!(
        r#"{{"url": "mssql://sa:pw@127.0.0.1:{}/app"{extra}}}"#,
        dead_port()
    ))
    .unwrap()
}

/// 建池是**惰性**的：`from_config` 不连库，`max_connections` 落到池的 `max_size`
/// 上，还没建过任何连接。
///
/// 这条同时把三个超时的配置钉住了 —— 设了超时又没给运行时的池会在 `build()` 里
/// 直接报 `NoRuntimeSpecified`，`from_config` 会变成 `Err`。
#[tokio::test]
async fn from_config_builds_a_lazy_pool_with_configured_size() {
    let client = MssqlClient::from_config(client_config(r#", "max_connections": 3"#))
        .await
        .unwrap();

    let status = client.pool_status();
    assert_eq!(status.max_size, 3);
    assert_eq!(status.size, 0, "建池不该立刻建连");
    assert_eq!(client.dialect(), Dialect::Mssql);
}

/// `min_connections: 0`（默认）时 `warm_up` 一条都不取 —— 连接串指向死端口也
/// 照样返回 `Ok`，证明它没有碰网络。
#[tokio::test]
async fn warm_up_takes_nothing_when_min_connections_is_zero() {
    let client = MssqlClient::from_config(client_config("")).await.unwrap();
    client.warm_up().await.unwrap();
    assert_eq!(client.pool_status().size, 0);
}

/// `min_connections > 0` 时 `warm_up` 真的去建连：连不上就是
/// [`RdbmsError::Connection`]，不能静默当成功（那会让启动期以为池子就绪了）。
#[tokio::test]
async fn warm_up_surfaces_connect_failure_as_connection_error() {
    let client = MssqlClient::from_config(client_config(r#", "min_connections": 1"#))
        .await
        .unwrap();

    // 超时只是保险：端口没人监听时 connect 立刻被拒。
    let err = tokio::time::timeout(Duration::from_secs(10), client.warm_up())
        .await
        .expect("不该挂住")
        .expect_err("端口没人监听，warm_up 必须失败");
    assert!(
        matches!(err, RdbmsError::Connection(_)),
        "应为 RdbmsError::Connection，got: {err:?}"
    );
}

/// 取不到连接 ≠ SQL 出错：查询路径上的连接失败也归 `Connection`。
#[tokio::test]
async fn query_maps_connect_failure_to_connection_error() {
    let client = MssqlClient::from_config(client_config("")).await.unwrap();

    let err = tokio::time::timeout(Duration::from_secs(10), client.query("SELECT 1"))
        .await
        .expect("不该挂住")
        .expect_err("端口没人监听，查询必须失败");
    assert!(
        matches!(err, RdbmsError::Connection(_)),
        "应为 RdbmsError::Connection，got: {err:?}"
    );
}

/// ADO 里用 `{}` 包住含 `;` 的值时本 crate 的扫描器读不准 —— 必须**不回答**
/// （`database` 留空，让 tiberius 解析出的 `my;db` 生效），不能拿被 `;` 切开的
/// 半截值 `my` 去覆盖：那会静默连到错误的库。
#[test]
fn ado_braced_value_is_not_guessed() {
    let c = MssqlConfig::from_str("Server=h;Database={my;db};User Id=sa").unwrap();
    assert_eq!(c.database, None, "花括号成对性不明确时不能猜");

    // 成对的 `{}` 与不带 `{}` 的值照常读得出来。
    let c = MssqlConfig::from_str("Server=h;Database={app}").unwrap();
    assert_eq!(c.database.as_deref(), Some("app"));
}

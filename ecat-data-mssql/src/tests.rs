// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `MssqlConfig` 的单元测试（不连库）。
//!
//! 独立成文件是因为 `config.rs` 有 500 行上限（与 `ecat-data-sqlx/src/tests.rs`
//! 同一个理由）；模块本身仍是 `#[cfg(test)] mod tests`，与内联无异。

use crate::MssqlConfig;
use std::time::Duration;

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

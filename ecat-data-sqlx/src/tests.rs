// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `SqlxClient` 的端到端测试。独立成文件是因为 `lib.rs` 有 500 行上限
//! （见计划「已知的 500 行超限」）；模块仍是 `#[cfg(test)] mod tests`，与
//! 内联无异。

use super::*;

#[test]
fn percent_encode_special_chars() {
    assert_eq!(percent_encode("user:pass"), "user%3Apass");
    assert_eq!(percent_encode("a/b@c"), "a%2Fb%40c");
    assert_eq!(percent_encode("a#b?c&d=e"), "a%23b%3Fc%26d%3De");
    assert_eq!(percent_encode("100%"), "100%25");
    assert_eq!(percent_encode("a+b"), "a%2Bb");
    assert_eq!(percent_encode("hello world"), "hello%20world");
}

#[test]
fn percent_encode_no_special_chars() {
    assert_eq!(percent_encode("simple"), "simple");
    assert_eq!(percent_encode("user123"), "user123");
    assert_eq!(percent_encode(""), "");
}

#[test]
fn config_deserialize_basic() {
    let cfg: SqlxConfig = serde_json::from_str(r#"{"url": "postgres://localhost/db"}"#).unwrap();
    assert_eq!(cfg.url, "postgres://localhost/db");
    assert!(cfg.username.is_none());
    assert!(cfg.password.is_none());
    assert!(cfg.tls.is_none());
}

#[test]
fn config_deserialize_with_auth() {
    let cfg: SqlxConfig = serde_json::from_str(
        r#"{"url": "mysql://localhost/db", "username": "root", "password": "secret"}"#,
    )
    .unwrap();
    assert_eq!(cfg.url, "mysql://localhost/db");
    assert_eq!(cfg.username.as_deref(), Some("root"));
    assert_eq!(cfg.password.as_deref(), Some("secret"));
}

#[test]
fn config_deserialize_with_tls() {
    let cfg: SqlxConfig =
        serde_json::from_str(r#"{"url": "postgres://localhost/db", "tls": {"skip_verify": true}}"#)
            .unwrap();
    assert!(cfg.tls.is_some());
    let tls = cfg.tls.unwrap();
    assert_eq!(tls.skip_verify, Some(true));
}

#[test]
fn config_missing_url_is_error() {
    let result: Result<SqlxConfig, _> = serde_json::from_str(r#"{}"#);
    assert!(result.is_err());
}

#[test]
fn from_pool_is_constructible() {
    // Compile-time check: SqlxClient::from_pool exists with correct signature.
    fn _check_sig(pool: Pool) -> SqlxClient {
        SqlxClient::from_pool(pool)
    }
}

/// 每次调用唯一的共享缓存内存库 URL。
fn mem_sqlite_url(name: &str) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("sqlite:ecat-test-{name}{n}?mode=memory&cache=shared")
}

/// 单连接池客户端：内存库随连接销毁，多连接池建的表会被后续连接丢失，
/// 单连接池保证同测试内所有语句命中同一库。
pub(crate) async fn mem_sqlite(name: &str) -> SqlxClient {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    SqlxClient::connect_with_params(&mem_sqlite_url(name), &params)
        .await
        .unwrap()
}

#[tokio::test]
async fn connect_and_execute_query_round_trip() {
    let client = mem_sqlite("t1").await;
    client
        .execute("CREATE TABLE t (id INTEGER, name TEXT)")
        .await
        .unwrap();
    client
        .execute("INSERT INTO t VALUES (1, 'alice'), (2, 'bob')")
        .await
        .unwrap();
    let rows = client
        .query("SELECT id, name FROM t ORDER BY id")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("id"), Some(&serde_json::json!(1)));
    assert_eq!(rows[0].get("name"), Some(&serde_json::json!("alice")));
    assert_eq!(rows[1].get("id"), Some(&serde_json::json!(2)));
    assert_eq!(rows[0].get("missing"), None);
}

#[tokio::test]
async fn execute_with_binds_all_json_value_types() {
    let client = mem_sqlite("t1").await;
    client
        .execute("CREATE TABLE t (s TEXT, i INTEGER, f REAL, b INTEGER, n TEXT)")
        .await
        .unwrap();
    let affected = client
        .execute_with(
            "INSERT INTO t VALUES (?, ?, ?, ?, ?)",
            &[
                serde_json::json!("str"),
                serde_json::json!(42),
                serde_json::json!(1.5),
                serde_json::json!(true),
                serde_json::Value::Null,
            ],
        )
        .await
        .unwrap();
    assert_eq!(affected, 1);
    let rows = client.query("SELECT * FROM t").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("s"), Some(&serde_json::json!("str")));
    assert_eq!(rows[0].get("i"), Some(&serde_json::json!(42)));
    assert_eq!(rows[0].get("f"), Some(&serde_json::json!(1.5)));
    // SQLite 无布尔类型：true 绑定后回读为整数 1（与 Any 驱动时代一致 ——
    // 原生驱动下 bool 若排在数值分支之前，这里会变成 `true`）
    assert_eq!(rows[0].get("b"), Some(&serde_json::json!(1)));
    assert_eq!(rows[0].get("n"), Some(&serde_json::Value::Null));
}

#[tokio::test]
async fn query_with_parameterized_sql() {
    let client = mem_sqlite("t1").await;
    client.execute("CREATE TABLE t (name TEXT)").await.unwrap();
    client
        .execute_with("INSERT INTO t VALUES (?)", &[serde_json::json!("x")])
        .await
        .unwrap();
    let rows = client
        .query_with(
            "SELECT name FROM t WHERE name = ?",
            &[serde_json::json!("x")],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&serde_json::json!("x")));
}

#[tokio::test]
async fn cell_to_json_encodes_blob_as_base64() {
    let client = mem_sqlite("t1").await;
    client.execute("CREATE TABLE t (data BLOB)").await.unwrap();
    // 真实 BLOB 字节（0x01 0x02 0x03），绕过 bind（bind 会把 JSON 值绑成文本）
    client
        .execute("INSERT INTO t VALUES (x'010203')")
        .await
        .unwrap();
    let rows = client.query("SELECT data FROM t").await.unwrap();
    assert_eq!(rows[0].get("data"), Some(&serde_json::json!("AQID")));
}

#[tokio::test]
async fn connect_with_auth_does_not_inject_into_non_url_scheme() {
    // sqlite URL 不含 "://"，凭据注入分支不生效，直接连接成功
    let client = SqlxClient::connect_with_auth(&mem_sqlite_url("t2"), "user", "pass")
        .await
        .unwrap();
    client.execute("SELECT 1").await.unwrap();
}

#[tokio::test]
async fn from_config_without_auth_connects_and_queries() {
    // 不手动准备任何驱动状态：验证 from_config 自身能在内存 sqlite 上
    // 实际执行查询（注意：多连接池下 mode=memory 库随连接关闭而销毁，
    // 故只做单条查询）
    let cfg = SqlxConfig {
        url: mem_sqlite_url("t3"),
        username: None,
        password: None,
        tls: None,
        session_init: None,
        max_connections: None,
        min_connections: None,
        acquire_timeout_secs: None,
        idle_timeout_secs: None,
        max_lifetime_secs: None,
        query_timeout_secs: None,
        test_before_acquire: None,
    };
    let client = SqlxClient::from_config(cfg).await.unwrap();
    let rows = client.query("SELECT 1 AS one").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("one"), Some(&serde_json::json!(1)));
}

/// 事务内可执行 SQL：参数化写入、回滚可见性、提交后可见性。
#[tokio::test]
async fn transaction_executes_and_scopes_changes() {
    let client = mem_sqlite("tx").await;
    client
        .execute("CREATE TABLE t (id INTEGER, name TEXT)")
        .await
        .unwrap();

    let tx = client.transaction().await.unwrap();
    assert_eq!(tx.dialect(), Dialect::Sqlite);
    assert_eq!(
        tx.execute_with(
            "INSERT INTO t VALUES (?, ?)",
            &[serde_json::json!(1), serde_json::json!("a")]
        )
        .await
        .unwrap(),
        1
    );
    let rows = tx
        .query_with("SELECT name FROM t WHERE id = ?", &[serde_json::json!(1)])
        .await
        .unwrap();
    assert_eq!(rows[0].get("name"), Some(&serde_json::json!("a")));
    tx.rollback().await.unwrap();
    assert!(client.query("SELECT id FROM t").await.unwrap().is_empty());

    let tx = client.transaction().await.unwrap();
    assert_eq!(
        tx.execute("INSERT INTO t VALUES (2, 'b')").await.unwrap(),
        1
    );
    assert_eq!(tx.query("SELECT id FROM t").await.unwrap().len(), 1);
    tx.commit().await.unwrap();
    let rows = client.query("SELECT name FROM t").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("name"), Some(&serde_json::json!("b")));
}

#[tokio::test]
async fn from_config_with_empty_credentials_connects_plain() {
    // (Some(""), Some("")) 或 (Some(""), None) 都走无认证分支
    for (u, p) in [
        (Some("".to_string()), Some("".to_string())),
        (Some("".to_string()), None),
    ] {
        let cfg = SqlxConfig {
            url: mem_sqlite_url("t3"),
            username: u,
            password: p,
            tls: None,
            session_init: None,
            max_connections: None,
            min_connections: None,
            acquire_timeout_secs: None,
            idle_timeout_secs: None,
            max_lifetime_secs: None,
            query_timeout_secs: None,
            test_before_acquire: None,
        };
        let client = SqlxClient::from_config(cfg).await.unwrap();
        client.execute("SELECT 1").await.unwrap();
    }
}

/// 方言由池的变体承载，不再靠 URL 猜测。
#[tokio::test]
async fn dialect_is_reported_from_pool_variant() {
    let db = mem_sqlite("dialect").await;
    assert_eq!(db.dialect(), Dialect::Sqlite);
}

#[tokio::test]
async fn transaction_rolls_back_on_drop() {
    let db = mem_sqlite("tx_rollback").await;
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
        .await
        .unwrap();
    let tx = db.transaction().await.unwrap();
    tx.execute("INSERT INTO t (id, v) VALUES (1, 'x')")
        .await
        .unwrap();
    drop(tx); // 未提交
    let rows = db.query("SELECT id FROM t").await.unwrap();
    assert!(rows.is_empty(), "未提交事务必须回滚");
}

#[tokio::test]
async fn transaction_commit_persists() {
    let db = mem_sqlite("tx_commit").await;
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
        .await
        .unwrap();
    let tx = db.transaction().await.unwrap();
    tx.execute("INSERT INTO t (id, v) VALUES (1, 'x')")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let rows = db.query("SELECT id FROM t").await.unwrap();
    assert_eq!(rows.len(), 1);
}

/// 客户端的 `execute_then_query` 覆写：**两条语句在同一个事务里跑完并提交**。
/// ORM 的 MySQL 两步式主键回填走这条路径（`LAST_INSERT_ID()` 是连接作用域的，
/// 两条语句必须落在同一条连接上）—— 不覆写就会走默认的「不支持」而报错。
#[tokio::test]
async fn execute_then_query_runs_both_statements_and_commits() {
    let db = mem_sqlite("exec_then_query").await;
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT NOT NULL)")
        .await
        .unwrap();

    let rows = db
        .execute_then_query(
            "INSERT INTO t (v) VALUES (?)",
            &[serde_json::json!("x")],
            "SELECT last_insert_rowid() AS id",
        )
        .await
        .unwrap();
    assert_eq!(
        rows.first().and_then(|r| r.get("id")),
        Some(&serde_json::json!(1)),
        "第二条语句必须看到第一条刚插入的行（同一条连接）"
    );

    // 已提交：若是「跑完没提交」，事务 drop 时会回滚，这里就是 0 行。
    let rows = db.query("SELECT count(*) AS n FROM t").await.unwrap();
    assert_eq!(rows[0].get("n"), Some(&serde_json::json!(1)));
}

/// 事务内的方言必须透传，否则 ORM 在事务里会生成错误占位符。
#[tokio::test]
async fn transaction_reports_dialect() {
    let db = mem_sqlite("tx_dialect").await;
    let tx = db.transaction().await.unwrap();
    assert_eq!(tx.dialect(), Dialect::Sqlite);
}

/// `query_timeout: None`（配置里的 `query_timeout_secs: 0`）时禁用超时。
#[tokio::test]
async fn zero_timeout_disables_timeout() {
    let params = PoolParams {
        max_connections: 1,
        query_timeout: None,
        ..PoolParams::default()
    };
    let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
        .await
        .unwrap();
    db.query("SELECT 1").await.unwrap();
}

/// 查询超时必须真的开火，**客户端与事务两条路径都覆盖**：`run_with_timeout` 的
/// 接线断了不会有别的东西变红 —— 慢查询会一直占着连接（见 `ecat-data/src/timeout.rs`）。
/// 5M 行递归 CTE 远慢于 100ms 的超时；超时丢掉的语句由 sqlx 中断，用例只花超时那点时间。
#[tokio::test]
async fn query_timeout_fires_on_client_and_transaction() {
    const SLOW: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 5000000) SELECT count(*) FROM c";
    let params = || PoolParams {
        max_connections: 1,
        query_timeout: Some(Duration::from_millis(100)),
        ..PoolParams::default()
    };

    // 事务路径（事务 wrapper 的接线是本次改动新加的）
    let db = SqlxClient::connect_with_params("sqlite::memory:", &params())
        .await
        .unwrap();
    let tx = db.transaction().await.unwrap();
    let err = tx.query(SLOW).await.expect_err("事务内查询必须超时");
    assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");

    // 客户端路径。另起一条池：上面那条连接刚被超时打断，状态不作数。
    let db = SqlxClient::connect_with_params("sqlite::memory:", &params())
        .await
        .unwrap();
    let err = db.query(SLOW).await.expect_err("客户端查询必须超时");
    assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
}

/// 预热必须真正建满 min_connections。
#[tokio::test]
async fn warm_up_creates_min_connections() {
    let params = PoolParams {
        max_connections: 4,
        min_connections: 3,
        ..PoolParams::default()
    };
    let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
        .await
        .unwrap();
    db.warm_up().await.unwrap();
    // 用 >= 而非 ==：sqlx 的后台保底任务可能同时在建连。
    assert!(
        db.pool_size() >= 3,
        "warm_up 后应至少有 3 条连接，实际 {}",
        db.pool_size()
    );
}

/// `min > max` 时 `warm_up()` 不能卡到 acquire_timeout：配置层把 min 夹到 max
/// （夹取本身在 config.rs 的用例里钉住，这里钉端到端行为；`acquire_timeout` 特意
/// 调到 1 秒，回归时会**快速失败**而不是默默等满 30 秒）。
#[tokio::test]
async fn warm_up_does_not_hang_when_min_exceeds_max() {
    let cfg: SqlxConfig = serde_json::from_str(
        r#"{"url": "sqlite::memory:", "max_connections": 2, "min_connections": 5,
            "acquire_timeout_secs": 1}"#,
    )
    .unwrap();
    let db = SqlxClient::from_config(cfg).await.unwrap();
    db.warm_up().await.unwrap();
    assert!(
        db.pool_size() >= 2,
        "warm_up 后应有 2 条连接，实际 {}",
        db.pool_size()
    );
}

// 真库（PG / MySQL）用例在 `live_tests.rs`：本文件已逼近 500 行上限。
#[tokio::test]
async fn warm_up_without_min_connections_is_a_noop() {
    let params = PoolParams {
        max_connections: 4,
        min_connections: 0,
        ..PoolParams::default()
    };
    let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
        .await
        .unwrap();
    // `connect()` 自身已经建好一条连接（sqlx 建池时会先连通一次），
    // 所以基线是 1 而不是 0；这里要钉的是「warm_up 不再多建」。
    let before = db.pool_size();
    db.warm_up().await.unwrap();
    assert_eq!(
        db.pool_size(),
        before,
        "min_connections = 0 时 warm_up 不应建连"
    );
}

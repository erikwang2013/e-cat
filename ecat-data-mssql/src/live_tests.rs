// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 真库（SQL Server）用例 —— 本 crate 里唯一真的走完 TDS 握手 + 登录 + 会话初始化
//! 的地方；`tests.rs` 的用例全部离线（建池、参数分派、连不上时的错误映射），
//! 类型映射与事务语义在那边复现不出来。
//!
//! 环境开关（与 `ecat-data-sqlx/src/live_tests.rs` 同一套）：
//! - `ECAT_TEST_MSSQL_URL`：真库连接串（URL 与 ADO 形态都行，见
//!   [`MssqlConfig::from_str`]）。
//! - `ECAT_REQUIRE_LIVE_DB`：设了（非空）时，「缺 URL」按**失败**处理而不是跳过
//!   —— CI 用这个开关把静默跳过变成红灯。
//!
//! **连本地的开发容器要用 ADO 形态带 `TrustServerCertificate=true`**：tiberius 默认
//! `EncryptionLevel::Required` 且校验证书，容器自签的证书过不了（rustls 实测报
//! `invalid peer certificate: Other(OtherError(UnsupportedCertVersion))`），而 URL
//! 形态**不支持查询串**（TLS 配置走 `tls` 字段，见 `MssqlConfig`）：
//!
//! ```text
//! ECAT_TEST_MSSQL_URL='Server=localhost,14333;User Id=sa;Password=Ecat_Test_2026!;TrustServerCertificate=true'
//! ```
//!
//! 例子里的 14333 是手工起的临时容器（避开宿主机上已占用的 1433）；根目录
//! `docker-compose.dev.yml` 的 mssql 服务映射的是 1433，把端口换掉即可。
//!
//! **`#tmp` 跨语句活不下来**（与 sqlx 侧相反）：tiberius 的 `query` / `execute`
//! 一律走 sp_executesql RPC，而动态批里建的临时表在**批结束时就没了** —— 实测
//! `EXEC sp_executesql N'CREATE TABLE #t (a int)'; SELECT ... FROM #t` 报
//! `Invalid object name '#t'`。所以：
//!
//! - 「建表 → 插 → 查」要么写在**同一条语句**里（参数绑定那条用例），
//! - 要么用 tempdb 里的**真实表**（事务用例；服务端重启即清，用例自己建自己删）。
//!
//! 事务语义只对**同一条连接**成立，所以事务用例既把池压到 `max_connections: 1`
//! （见 [`live_client`]），又用 `@@SPID` 把「还是那条连接」变成断言（见 [`spid`]）——
//! 池子换了连接的话，「未提交的数据别的连接看不见」会让断言白白通过。
//!
//! [`pool_create_timeout_bounds_create`] 不需要真库，因此**不受上面两个开关影响**。

use super::*;
use crate::MssqlConfig;
use ecat_data::{RdbmsClient, RdbmsError, SqlExecutor};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// 真库连接串的跳过策略：
/// - 未设 `ECAT_TEST_MSSQL_URL` 且未设 `ECAT_REQUIRE_LIVE_DB` → 跳过（本地开发）。
/// - 未设 `ECAT_TEST_MSSQL_URL` 但设了 `ECAT_REQUIRE_LIVE_DB` → **panic**。
///
/// 为什么要有这道闸门：cargo 默认捕获 stdout，`println!` 的跳过提示看不见 ——
/// 「静默跳过」在 CI 里等于没验证，必须能变成红灯。
fn live_db_url() -> Option<String> {
    match std::env::var("ECAT_TEST_MSSQL_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            if std::env::var("ECAT_REQUIRE_LIVE_DB").is_ok_and(|v| !v.is_empty()) {
                panic!(
                    "ECAT_TEST_MSSQL_URL 未设，但 ECAT_REQUIRE_LIVE_DB 要求真库用例必须运行（跳过即失败）"
                );
            }
            println!("skip: ECAT_TEST_MSSQL_URL 未设置");
            None
        }
    }
}

/// 单连接池客户端：事务用例要的是「几条语句都落在同一条连接上」。
async fn live_client(url: &str) -> MssqlClient {
    let mut cfg = MssqlConfig::from_str(url).unwrap();
    cfg.max_connections = Some(1);
    MssqlClient::from_config(cfg).await.unwrap()
}

/// 会话 id。事务用例靠它把「还是开事务的那条连接」变成断言 —— 池子
/// `max_connections: 1` 时不换连接只是**假设**，换了的话下面「未提交的数据看不见」
/// 一类断言会白白通过。
async fn spid(db: &MssqlClient) -> i64 {
    let rows = db.query("SELECT @@SPID AS spid").await.unwrap();
    rows[0].get("spid").and_then(Value::as_i64).unwrap()
}

/// `SELECT COUNT(*)` → i64：事务用例里反复用它看表的可见行数。
async fn count_rows(db: &MssqlClient, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) AS n FROM {table}");
    let rows = db.query(sql.as_str()).await.unwrap();
    rows[0].get("n").and_then(Value::as_i64).unwrap()
}

/// 事务用例的落地表：**tempdb 里的真实表**（理由见文件头：`#tmp` 跨语句活不下来）。
/// tempdb 是服务端的草稿空间，服务端重启即清；用例自己建、自己删，建之前先删残留。
async fn create_tx_table(db: &MssqlClient, table: &str) {
    db.execute(&format!("DROP TABLE IF EXISTS {table}"))
        .await
        .unwrap();
    db.execute(&format!("CREATE TABLE {table} (v INT)"))
        .await
        .unwrap();
}

/// 未提交即 drop 的那条用例的表名（commit 那条用另一个，两条用例互不干扰）。
const TX_TABLE_DROP: &str = "tempdb.dbo.ecat_t6_tx_drop";
/// 提交/回滚那条用例的表名。
const TX_TABLE_COMMIT: &str = "tempdb.dbo.ecat_t6_tx_commit";

/// 类型映射逐个实测（批次 1 在 sqlx 路径上付过学费的地方）。
///
/// - 整数四个宽度、`BIT`、`NVARCHAR`、`UNIQUEIDENTIFIER`、`VARBINARY`(base64)
///   各归各的；
/// - **`DATE` 出纯日期** `"2026-10-05"`，不发明「UTC 午夜」这个时刻；
/// - **`DATETIME2` 出 RFC3339 UTC**；
/// - **`DECIMAL` 响亮报错**（不是静默 null），消息带列名与类型名；
/// - NULL 在每个类型上都还是 NULL —— **包括 `DECIMAL` 的 NULL**：NULL 是
///   「没有值」，不是「表示形态未定」，不该跟着一起报错。
#[tokio::test]
async fn native_types_decode_and_decimal_errors() {
    let Some(url) = live_db_url() else {
        return;
    };
    let db = MssqlClient::connect(&url).await.unwrap();

    // 日期用 CONVERT 的显式 style（23 = yyyy-mm-dd，126 = ISO8601）：`CAST('...')`
    // 的字面量解释随 DATEFORMAT 变，容器的默认语言是 us_english。
    let rows = db
        .query(
            "SELECT CAST(200 AS TINYINT) AS t_u8, \
                    CAST(-30000 AS SMALLINT) AS t_i16, \
                    CAST(2000000000 AS INT) AS t_i32, \
                    CAST(9000000000000000000 AS BIGINT) AS t_i64, \
                    CAST(1 AS BIT) AS t_bit, \
                    CAST(N'中文' AS NVARCHAR(50)) AS t_str, \
                    CONVERT(DATE, '2026-10-05', 23) AS t_date, \
                    CONVERT(DATETIME2, '2026-10-05T12:34:56', 126) AS t_dt2, \
                    CAST('67e55044-10b1-426f-9247-bb680e5fe0c8' AS UNIQUEIDENTIFIER) AS t_guid, \
                    CAST(0x0102FF AS VARBINARY(3)) AS t_bin",
        )
        .await
        .unwrap();
    let r = &rows[0];
    assert_eq!(r.get("t_u8"), Some(&json!(200)), "TINYINT");
    assert_eq!(r.get("t_i16"), Some(&json!(-30000)), "SMALLINT");
    assert_eq!(r.get("t_i32"), Some(&json!(2000000000)), "INT");
    assert_eq!(
        r.get("t_i64"),
        Some(&json!(9000000000000000000i64)),
        "BIGINT"
    );
    assert_eq!(r.get("t_bit"), Some(&json!(true)), "BIT 该是布尔");
    assert_eq!(r.get("t_str"), Some(&json!("中文")), "NVARCHAR");
    // 批次 1 的学费之一：DATE 补成 UTC 午夜瞬时是凭空断言。
    assert_eq!(
        r.get("t_date"),
        Some(&json!("2026-10-05")),
        "DATE 该是纯日期"
    );
    // 无时区的 DATETIME2 按既有约定当 UTC，输出 RFC3339。
    assert_eq!(
        r.get("t_dt2"),
        Some(&json!("2026-10-05T12:34:56Z")),
        "DATETIME2 该是 RFC3339 UTC"
    );
    assert_eq!(
        r.get("t_guid"),
        Some(&json!("67e55044-10b1-426f-9247-bb680e5fe0c8")),
        "UNIQUEIDENTIFIER 该是带连字符的小写串"
    );
    assert_eq!(
        r.get("t_bin"),
        Some(&json!("AQL/")),
        "VARBINARY 该是 base64"
    );

    // DECIMAL：报错而非静默 null，且要点名是哪个列、什么类型。
    let msg = db
        .query("SELECT CAST(1.5 AS DECIMAL(10,2)) AS amount")
        .await
        .unwrap_err()
        .to_string();
    assert!(msg.contains("unsupported column type"), "{msg}");
    assert!(msg.contains("amount"), "错误信息要带列名: {msg}");
    assert!(msg.contains("NUMERIC"), "错误信息要带类型名: {msg}");

    // NULL：每个变体的 `None` 都是 NULL，不需要额外的闸门。
    let rows = db
        .query(
            "SELECT CAST(NULL AS TINYINT) AS n_u8, \
                    CAST(NULL AS INT) AS n_i32, \
                    CAST(NULL AS BIT) AS n_bit, \
                    CAST(NULL AS NVARCHAR(10)) AS n_str, \
                    CAST(NULL AS DATE) AS n_date, \
                    CAST(NULL AS DATETIME2) AS n_dt2, \
                    CAST(NULL AS UNIQUEIDENTIFIER) AS n_guid, \
                    CAST(NULL AS VARBINARY(3)) AS n_bin, \
                    CAST(NULL AS DECIMAL(10,2)) AS n_dec, \
                    CAST(NULL AS XML) AS n_xml",
        )
        .await
        .unwrap();
    for col in [
        "n_u8", "n_i32", "n_bit", "n_str", "n_date", "n_dt2", "n_guid", "n_bin", "n_dec", "n_xml",
    ] {
        assert_eq!(rows[0].get(col), Some(&Value::Null), "{col} 该是 NULL");
    }
}

/// 参数绑定 `@P1` / `@P2` 往返：中文（UTF-16 走 NVARCHAR）与 NULL。
///
/// 「建表 → 插 → 查」写在**同一条语句**里：`#t6_bind` 是临时表，跨语句就没了
/// （理由见文件头）。这也顺带钉住「多语句批里 `into_first_result` 拿到的是 SELECT
/// 那个结果集」。
///
/// NULL 那一格顺带验了 [`crate::bind`] 的选择：JSON null 不带类型、TDS 参数却是
/// 强类型的，落成 nvarchar 的 NULL 后**隐式转换**到目标列（这里是个 INT 列）——
/// 服务端语义仍是 NULL 而不是 `'NULL'` 之类的文本。
#[tokio::test]
async fn parameter_binding_round_trips() {
    let Some(url) = live_db_url() else {
        return;
    };
    let db = MssqlClient::connect(&url).await.unwrap();

    let rows = db
        .query_with(
            "CREATE TABLE #t6_bind (s NVARCHAR(100), n INT); \
             INSERT INTO #t6_bind VALUES (@P1, @P2); \
             SELECT s, n FROM #t6_bind",
            &[json!("中文"), Value::Null],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "该只拿到 SELECT 的结果集");
    assert_eq!(rows[0].get("s"), Some(&json!("中文")));
    assert_eq!(rows[0].get("n"), Some(&Value::Null));

    // 不落表的纯往返：五种 JSON 形态各自的变体都完整回来。
    let rows = db
        .query_with(
            "SELECT @P1 AS s, @P2 AS n, @P3 AS i, @P4 AS b, @P5 AS f",
            &[
                json!("中文"),
                Value::Null,
                json!(7),
                json!(true),
                json!(1.5),
            ],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get("s"), Some(&json!("中文")));
    assert_eq!(rows[0].get("n"), Some(&Value::Null));
    assert_eq!(rows[0].get("i"), Some(&json!(7)));
    assert_eq!(rows[0].get("b"), Some(&json!(true)));
    assert_eq!(rows[0].get("f"), Some(&json!(1.5)));
}

/// 未提交即 drop → **回滚**，且连接归还时是干净的。
///
/// tiberius 没有事务对象，wrapper 被 drop 时在 Drop 里没法 await 回滚 —— 清脏只能
/// 落在取用路径上：`MssqlManager::recycle` 依据连接上的 `in_transaction` 标记发
/// `IF @@TRANCOUNT > 0 ROLLBACK`。这条用例是那个设计唯一的真库证据。
#[tokio::test]
async fn transaction_drop_without_commit_rolls_back() {
    let Some(url) = live_db_url() else {
        return;
    };
    let db = live_client(&url).await;
    create_tx_table(&db, TX_TABLE_DROP).await;
    let before = spid(&db).await;

    let n = {
        let tx = db.transaction().await.unwrap();
        tx.execute(&format!("INSERT INTO {TX_TABLE_DROP} VALUES (1)"))
            .await
            .unwrap();
        // 事务里先确认那行真进去了（否则下面的「回滚掉了」是空的）。事务持着唯一的
        // 连接，事务内的语句必须走 tx —— 走 db 会等连接一直等到 acquire_timeout。
        let sql = format!("SELECT COUNT(*) AS n FROM {TX_TABLE_DROP}");
        let rows = tx.query(sql.as_str()).await.unwrap();
        rows[0].get("n").and_then(Value::as_i64).unwrap()
        // tx 在此 drop：既没 commit 也没 rollback。
    };
    assert_eq!(n, 1, "事务内该看得见自己插的行");

    assert_eq!(spid(&db).await, before, "事务之后该还是同一条连接");
    assert_eq!(
        count_rows(&db, TX_TABLE_DROP).await,
        0,
        "未提交的事务必须被回滚掉"
    );
    // 标记生效的直接证据：连接上不再有开着的事务。没清脏的话这里是 1，而且接下来
    // 所有语句都会被吸进那个事务。
    let rows = db.query("SELECT @@TRANCOUNT AS t").await.unwrap();
    assert_eq!(rows[0].get("t"), Some(&json!(0)), "@@TRANCOUNT 该是 0");

    db.execute(&format!("DROP TABLE {TX_TABLE_DROP}"))
        .await
        .unwrap();
}

/// commit → 可见；显式 rollback → 不可见（与 drop 是两条不同的代码路径）。
#[tokio::test]
async fn transaction_commit_is_visible_and_rollback_is_not() {
    let Some(url) = live_db_url() else {
        return;
    };
    let db = live_client(&url).await;
    create_tx_table(&db, TX_TABLE_COMMIT).await;
    let before = spid(&db).await;

    let tx = db.transaction().await.unwrap();
    tx.execute(&format!("INSERT INTO {TX_TABLE_COMMIT} VALUES (1)"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(spid(&db).await, before, "事务之后该还是同一条连接");
    assert_eq!(count_rows(&db, TX_TABLE_COMMIT).await, 1, "提交后该可见");

    let tx = db.transaction().await.unwrap();
    tx.execute(&format!("INSERT INTO {TX_TABLE_COMMIT} VALUES (2)"))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(count_rows(&db, TX_TABLE_COMMIT).await, 1, "回滚后不该可见");

    db.execute(&format!("DROP TABLE {TX_TABLE_COMMIT}"))
        .await
        .unwrap();
}

/// `warm_up` 真的建满 `min_connections`：deadpool 没有保底连接的概念，取完放回
/// 后池里就真躺着这么多条建好的连接。
#[tokio::test]
async fn warm_up_builds_min_connections() {
    let Some(url) = live_db_url() else {
        return;
    };
    let mut cfg = MssqlConfig::from_str(&url).unwrap();
    cfg.max_connections = Some(3);
    cfg.min_connections = Some(3);
    let db = MssqlClient::from_config(cfg).await.unwrap();

    db.warm_up().await.unwrap();
    let status = db.pool_status();
    assert_eq!(status.size, 3, "池里该有 3 条建好的连接");
    assert_eq!(status.available, 3, "warm_up 结束后全部是空闲的");
}

/// 会话初始化真的作用在**会话**上：`SET ARITHABORT ON` 走的是 batch
/// （`simple_query`）而不是 sp_executesql RPC —— SET 选项在 RPC 作用域结束时会被
/// 还原，之后每条查询都会重演「计划缓存被两种 ARITHABORT 互相挤掉」的问题。
///
/// 这条用例就是那个判断的真库验证：查的是 `SESSIONPROPERTY`（会话级状态），
/// 不是当前批的状态。
#[tokio::test]
async fn session_init_opens_arithabort() {
    let Some(url) = live_db_url() else {
        return;
    };
    let db = MssqlClient::connect(&url).await.unwrap();

    let rows = db
        .query("SELECT SESSIONPROPERTY('ARITHABORT') AS v")
        .await
        .unwrap();
    assert_eq!(
        rows[0].get("v"),
        Some(&json!(1)),
        "会话初始化没把 ARITHABORT 设上"
    );
}

/// 池的三个超时里唯一能在进程内验的一条：`create_timeout`（建连 30s 上限）。
///
/// **不连真库**，所以不受 `ECAT_TEST_MSSQL_URL` 门控 —— 任何环境下都跑。
///
/// 两段：
/// 1. 死端口（无人监听）：connect 立刻被拒，走 `PoolError::Backend` 分支。
/// 2. 黑洞端口（accept 但永不回 prelogin）：tiberius 自己没有握手超时，能结束
///    建连的只有池的 `create_timeout` —— 断言它**等了约 30s 才失败**，而不是
///    无限等（`recycle` 是在 `Pool::get()` 路径上同步等的，一次卡死的建连会把
///    取用拖到 `wait_timeout`）。
///
/// 第 2 段关掉 `query_timeout_secs`：查询超时也是 30s，同样从 `pool.get()` 起算，
/// 不关的话会抢在 `create_timeout` 前面报 `Timeout`，验不到池那一层。
#[tokio::test]
async fn pool_create_timeout_bounds_create() {
    // ① 死端口：先占再释放，得到一个必然没人监听的地址（不依赖外部环境）。
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let db = MssqlClient::connect(&format!("mssql://sa:pw@127.0.0.1:{dead_port}"))
        .await
        .unwrap();
    let started = Instant::now();
    let err = db.query("SELECT 1").await.unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        matches!(err, RdbmsError::Connection(_)),
        "取连接失败该是 Connection，got: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "死端口该立刻失败，实际 {elapsed:?}"
    );

    // ② 黑洞端口：收下连接就不管，永不回 prelogin 响应。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let cfg: MssqlConfig = serde_json::from_str(&format!(
        r#"{{"url": "mssql://sa:pw@127.0.0.1:{}", "query_timeout_secs": 0}}"#,
        addr.port()
    ))
    .unwrap();
    let db = MssqlClient::from_config(cfg).await.unwrap();

    let started = Instant::now();
    let err = db.query("SELECT 1").await.unwrap_err();
    let elapsed = started.elapsed();
    let msg = err.to_string();
    assert!(
        matches!(err, RdbmsError::Connection(_)),
        "建连超时该是 Connection，got: {err:?}"
    );
    assert!(
        msg.to_lowercase().contains("timeout"),
        "该是池的建连超时（CREATE_TIMEOUT = 30s），got: {msg}"
    );
    assert!(
        elapsed >= Duration::from_secs(25),
        "该耗到 30s 的 create_timeout，实际只有 {elapsed:?}（说明不是它结束的建连）"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "建连没被 create_timeout 掐住，实际 {elapsed:?}"
    );
}

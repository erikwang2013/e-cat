// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 真库（PG / MySQL）用例。与 `tests.rs` 分开是因为它已逼近 500 行上限
//! （项目规则：每个文件 < 500 行），模块本身仍是 `#[cfg(test)]`。
//!
//! 环境开关：
//! - `ECAT_TEST_PG_URL` / `ECAT_TEST_MYSQL_URL`：真库连接串。
//! - `ECAT_REQUIRE_LIVE_DB`：设了（非空）时，「缺 URL」按**失败**处理而不是跳过
//!   —— CI 用这个开关把静默跳过变成红灯。

use super::*;

/// 真库连接串。两个开关的语义：
/// - 未设 `ECAT_TEST_PG_URL` / `ECAT_TEST_MYSQL_URL` 且未设 `ECAT_REQUIRE_LIVE_DB`
///   → 打印一行并跳过（本地开发）。
/// - 设了 `ECAT_REQUIRE_LIVE_DB`（非空）却没有 URL → **panic**。CI 里「静默跳过」
///   等于没验证，而 cargo 默认捕获 stdout，`println!` 的跳过提示看不见。
///
/// 为什么要这道闸门：**本地 SQLite 全绿不能冒充三驱动验证** ——
/// `int2` / `UNSIGNED` / `real` 这些静默 null 在 SQLite 上根本复现不出来。
fn live_db_url(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            if std::env::var("ECAT_REQUIRE_LIVE_DB").is_ok_and(|v| !v.is_empty()) {
                panic!("{key} 未设，但 ECAT_REQUIRE_LIVE_DB 要求真库用例必须运行（跳过即失败）");
            }
            println!("skip: {key} 未设置");
            None
        }
    }
}

/// PG 的 `smallint`(int2) / `real`(float4) / `date` / `timestamp` 都要走原生分支；
/// 链外类型（numeric/uuid/jsonb…）必须报错而不是静默 null。
#[tokio::test]
async fn pg_native_types_decode_and_unsupported_type_errors() {
    let Some(url) = live_db_url("ECAT_TEST_PG_URL") else {
        return;
    };
    let db = SqlxClient::connect(&url).await.unwrap();
    let rows = db
        .query(
            "SELECT 7::int2 AS s, 3000000000::int8 AS b, 1.5::float4 AS r, \
             0.1::float4 AS r2, DATE '2026-10-05' AS d, \
             TIMESTAMP '2026-10-05 12:34:56' AS ts",
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get("s"), Some(&serde_json::json!(7)), "int2 落空");
    assert_eq!(rows[0].get("b"), Some(&serde_json::json!(3000000000i64)));
    assert_eq!(
        rows[0].get("r"),
        Some(&serde_json::json!(1.5)),
        "float4 落空"
    );
    // 最短往返表示：直接 `as f64` 会得到 0.10000000149011612
    assert_eq!(
        rows[0].get("r2"),
        Some(&serde_json::json!(0.1)),
        "float4 不是最短表示"
    );
    // `date` 只输出日期，不发明「UTC 午夜」这个时刻
    assert_eq!(rows[0].get("d"), Some(&serde_json::json!("2026-10-05")));
    assert_eq!(
        rows[0].get("ts"),
        Some(&serde_json::json!("2026-10-05T12:34:56Z"))
    );

    let msg = db
        .query("SELECT 1.5::numeric AS n")
        .await
        .unwrap_err()
        .to_string();
    assert!(msg.contains("unsupported column type"), "{msg}");
    // 类型名来自 `PgTypeInfo::name()`，实测是大写 `NUMERIC`；大小写不敏感地断言。
    assert!(
        msg.to_uppercase().contains("(NUMERIC)"),
        "错误信息要带类型名: {msg}"
    );
}

/// MySQL 的 `* UNSIGNED` 列（含最常见自增主键 `BIGINT UNSIGNED`）不能被 bool
/// 分支吃掉、也不能落成 null；`DECIMAL` 等链外类型必须报错。
#[tokio::test]
async fn mysql_unsigned_columns_decode_and_unsupported_type_errors() {
    let Some(url) = live_db_url("ECAT_TEST_MYSQL_URL") else {
        return;
    };
    // 单连接池：临时表是会话级的，多连接池下 CREATE / INSERT / SELECT 会落到
    // 不同连接上（实测报 `Table doesn't exist`）。
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::for_url(&url)
    };
    let db = SqlxClient::connect_with_params(&url, &params)
        .await
        .unwrap();
    db.execute("CREATE TEMPORARY TABLE t6_unsigned (big BIGINT UNSIGNED, tiny TINYINT UNSIGNED)")
        .await
        .unwrap();
    db.execute("INSERT INTO t6_unsigned VALUES (5000000000, 128)")
        .await
        .unwrap();
    let rows = db.query("SELECT big, tiny FROM t6_unsigned").await.unwrap();
    assert_eq!(rows[0].get("big"), Some(&serde_json::json!(5000000000u64)));
    assert_eq!(rows[0].get("tiny"), Some(&serde_json::json!(128)));

    // 实测（真 MySQL 8.0）：`BOOLEAN` 就是 `TINYINT(1)`，被整数分支先接住 → 数字 1
    // （SQLite 侧同样如此，见 `execute_with_binds_all_json_value_types`；Any 时代
    // `ColumnType::Tiny` 不在 Any 的类型表里，这一列会让**整条查询报错**）。
    // `BIT(1)` 由 u64 分支按整数解出（`uint_compatible` 含 Bit），不再静默 null。
    db.execute("CREATE TEMPORARY TABLE t6_types (flag BOOLEAN, bits BIT(1), d DATE)")
        .await
        .unwrap();
    db.execute("INSERT INTO t6_types VALUES (TRUE, b'1', '2026-10-05')")
        .await
        .unwrap();
    let rows = db
        .query("SELECT flag, bits, d FROM t6_types")
        .await
        .unwrap();
    assert_eq!(rows[0].get("flag"), Some(&serde_json::json!(1)));
    assert_eq!(rows[0].get("bits"), Some(&serde_json::json!(1)));
    // MySQL 的 DATE 只有 Date 分支认（`OffsetDateTime`/`PrimitiveDateTime` 的
    // compatible 只含 Datetime/Timestamp），输出纯日期。
    assert_eq!(rows[0].get("d"), Some(&serde_json::json!("2026-10-05")));

    let msg = db
        .query("SELECT CAST(1.5 AS DECIMAL(10,2)) AS d")
        .await
        .unwrap_err()
        .to_string();
    assert!(msg.contains("unsupported column type"), "{msg}");
}

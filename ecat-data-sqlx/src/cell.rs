// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use base64::Engine as _;
use ecat_data::Row;
use sqlx::{Column as SqlxColumn, Row as SqlxRow, ValueRef as SqlxValueRef};

/// 三种驱动的 Row 类型不同，用宏生成三份同构实现。
///
/// 类型链：NULL → i64 → i32 → f64（NaN/Inf 转字符串）→ bool →
/// OffsetDateTime（→ RFC3339 UTC）→ String → Blob（base64）→ Null。
///
/// 链序两处与 Any 驱动时代的直觉不同，都是原生驱动逼出来的：
/// 1. **NULL 先拦**：sqlite 的 `bool::decode` 直通 C API `sqlite3_value_int64`，
///    对 NULL 返回 0 而不报错，少了这道闸门 NULL 会静默变成 `false`
///    （PG/MySQL 在 decode 时报错，无此问题，闸门对它们只是短路）。
/// 2. **bool 在数值之后**：sqlite 的 `bool::compatible` 连 `Int4 | Integer`
///    一起算兼容，MySQL 的含全部整数列类型（Tiny/Short/Long/Int24/LongLong/Bit），
///    而两者的 `bool::decode` 都是「非 0 即真」。bool 若排在最前，任意整数
///    （如 `SELECT 42`）都会变成 `true`；数值分支先接住则与 Any 驱动时代一致。
macro_rules! cell_fn {
    ($name:ident, $row:ty) => {
        pub fn $name(row: &$row, col: &str) -> serde_json::Value {
            if row.try_get_raw(col).is_ok_and(|v| v.is_null()) {
                return serde_json::Value::Null;
            }
            row.try_get::<i64, _>(col)
                .map(|n| serde_json::Value::Number(n.into()))
                .or_else(|_| {
                    row.try_get::<i32, _>(col)
                        .map(|n| serde_json::Value::Number((n as i64).into()))
                })
                .or_else(|_| {
                    row.try_get::<f64, _>(col)
                        .ok()
                        .and_then(|n| {
                            if n.is_finite() {
                                serde_json::Number::from_f64(n).map(serde_json::Value::Number)
                            } else if n.is_nan() {
                                Some(serde_json::Value::String("NaN".into()))
                            } else if n > 0.0 {
                                Some(serde_json::Value::String("Infinity".into()))
                            } else {
                                Some(serde_json::Value::String("-Infinity".into()))
                            }
                        })
                        .ok_or(())
                })
                .or_else(|_| row.try_get::<bool, _>(col).map(serde_json::Value::Bool))
                // 时间分支：原生池支持 time 类型，这正是弃用 Any 驱动的收益。
                // 统一转 UTC，避免带偏移量的字符串破坏排序。
                .or_else(|_| {
                    row.try_get::<time::OffsetDateTime, _>(col).map(|dt| {
                        let dt = dt.to_offset(time::UtcOffset::UTC);
                        serde_json::Value::String(
                            dt.format(&time::format_description::well_known::Rfc3339)
                                .unwrap_or_else(|_| dt.to_string()),
                        )
                    })
                })
                .or_else(|_| row.try_get::<String, _>(col).map(serde_json::Value::String))
                .or_else(|_| {
                    row.try_get::<Vec<u8>, _>(col).map(|b| {
                        serde_json::Value::String(
                            base64::engine::general_purpose::STANDARD.encode(b),
                        )
                    })
                })
                .unwrap_or(serde_json::Value::Null)
        }
    };
}

cell_fn!(pg_cell_to_json, sqlx::postgres::PgRow);
cell_fn!(mysql_cell_to_json, sqlx::mysql::MySqlRow);
cell_fn!(sqlite_cell_to_json, sqlx::sqlite::SqliteRow);

macro_rules! rows_fn {
    ($name:ident, $row:ty, $cell:ident) => {
        pub fn $name(rows: Vec<$row>) -> Vec<Row> {
            if rows.is_empty() {
                return Vec::new();
            }
            let columns: Vec<String> = rows[0]
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect();
            rows.iter()
                .map(|row| {
                    let values: Vec<serde_json::Value> =
                        columns.iter().map(|col| $cell(row, col)).collect();
                    Row::new(columns.clone(), values)
                })
                .collect()
        }
    };
}

rows_fn!(pg_rows_to_result, sqlx::postgres::PgRow, pg_cell_to_json);
rows_fn!(
    mysql_rows_to_result,
    sqlx::mysql::MySqlRow,
    mysql_cell_to_json
);
rows_fn!(
    sqlite_rows_to_result,
    sqlx::sqlite::SqliteRow,
    sqlite_cell_to_json
);

#[cfg(test)]
mod tests {
    use crate::tests::mem_sqlite;
    use ecat_data::SqlExecutor;

    /// 时间分支必须真的被走到：带偏移量的文本经 `time` 解码后统一转 UTC。
    /// 若落到 String 分支，`+08:00` 会原样保留（见第二个断言）。
    #[tokio::test]
    async fn datetime_round_trips_as_rfc3339() {
        let db = mem_sqlite("cell_datetime").await;
        db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, at TEXT NOT NULL)")
            .await
            .unwrap();
        db.execute_with(
            "INSERT INTO t (id, at) VALUES (?, ?)",
            &[
                serde_json::json!(1),
                serde_json::json!("2026-10-05T12:34:56+08:00"),
            ],
        )
        .await
        .unwrap();
        let rows = db.query("SELECT at FROM t").await.unwrap();
        let value = rows[0].get("at").unwrap().as_str().unwrap();
        assert!(value.starts_with("2026-10-05"), "got: {value}");
        assert_eq!(value, "2026-10-05T04:34:56Z", "时间分支未生效: {value}");
    }

    #[tokio::test]
    async fn bytes_become_base64() {
        let db = mem_sqlite("cell_bytes").await;
        db.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, raw BLOB)")
            .await
            .unwrap();
        db.execute("INSERT INTO b (id, raw) VALUES (1, X'0102FF')")
            .await
            .unwrap();
        let rows = db.query("SELECT raw FROM b").await.unwrap();
        assert_eq!(rows[0].get("raw").unwrap().as_str().unwrap(), "AQL/");
    }

    #[tokio::test]
    async fn null_survives_conversion() {
        let db = mem_sqlite("cell_null").await;
        db.execute("CREATE TABLE n (id INTEGER PRIMARY KEY, v TEXT)")
            .await
            .unwrap();
        db.execute("INSERT INTO n (id, v) VALUES (1, NULL)")
            .await
            .unwrap();
        let rows = db.query("SELECT v FROM n").await.unwrap();
        assert!(rows[0].get("v").unwrap().is_null());
    }

    /// 整数列必须是数字，不能被 bool 分支吃掉（sqlite 的 `bool::compatible`
    /// 把 Integer 也算兼容，bool 排最前时这里会得到 `true`）。
    #[tokio::test]
    async fn integers_stay_numbers() {
        let db = mem_sqlite("cell_int").await;
        let rows = db.query("SELECT 42 AS n").await.unwrap();
        assert_eq!(rows[0].get("n"), Some(&serde_json::json!(42)));
    }
}

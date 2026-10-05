// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use base64::Engine as _;
use ecat_data::{RdbmsError, Row};
use sqlx::{Column as SqlxColumn, Row as SqlxRow, TypeInfo as _, ValueRef as SqlxValueRef};

/// 浮点转 JSON：`serde_json::Number` 装不下 NaN/±Inf，转字符串
/// （沿用 Any 驱动时代的表示）。
fn float_to_json(n: f64) -> serde_json::Value {
    if n.is_finite() {
        serde_json::Number::from_f64(n).map_or(serde_json::Value::Null, serde_json::Value::Number)
    } else if n.is_nan() {
        serde_json::Value::String("NaN".into())
    } else if n > 0.0 {
        serde_json::Value::String("Infinity".into())
    } else {
        serde_json::Value::String("-Infinity".into())
    }
}

/// `f32` → JSON：先过一趟 f32 的**最短往返十进制**表示再解析回 f64。
///
/// f32 的 `Display` 用的是最短往返算法（`0.1f32.to_string()` 得 `"0.1"`）；
/// 直接 `n as f64` 会把 `0.1` 加宽成 `0.10000000149011612` —— 数值上没错，
/// 但那是二进制加宽的原样值，不是这个 f32 的语义。
/// `NaN`/`inf` 的 `Display` 解析不回数字，回落到 [`float_to_json`] 的字符串表示。
fn f32_to_json(n: f32) -> serde_json::Value {
    match n.to_string().parse::<f64>() {
        Ok(v) => float_to_json(v),
        Err(_) => float_to_json(n as f64),
    }
}

/// 带时区的时间统一转 UTC 再按 RFC3339 输出。
///
/// `time` 的 `Display` **不是** RFC3339（形如 `2026-10-05 12:34:56.0 +00:00:00`，
/// 且带偏移量、小时位不补零），必须显式 `format(&Rfc3339)`；ORM 侧按 RFC3339 解析。
///
/// 格式化失败**报错**而不是回落 `to_string()`：那样只会溜出一个格式不对的
/// 字符串喂给按 RFC3339 解析的调用方。超范围年份（RFC3339 只认 `0000-9999`）
/// 就该在这里响亮地失败。
fn rfc3339(dt: time::OffsetDateTime, col: &str) -> Result<String, RdbmsError> {
    dt.to_offset(time::UtcOffset::UTC)
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| {
            RdbmsError::Database(format!(
                "failed to format datetime column {col} as RFC3339: {e}"
            ))
        })
}

/// 三种驱动的 Row 类型不同，用宏生成三份同构实现。
///
/// 类型链（逐个试，先命中先返回）：NULL → i64 → i32 → i16 → f64 → f32 →
/// [u64] → bool → OffsetDateTime → PrimitiveDateTime → Date → String →
/// Blob（base64）→ **报错**。
///
/// 四处与 Any 驱动时代不同，都是原生驱动逼出来的（Any 的 `compatible` 是一张
/// 宽松的兼容表，原生驱动是**精确匹配 + 标志位**）：
/// 1. **NULL 先拦**：sqlite 的 `bool::decode` 直通 C API `sqlite3_value_int64`，
///    对 NULL 返回 0 而不报错，少了这道闸门 NULL 会静默变成 `false`
///    （PG/MySQL 在 decode 时报错，无此问题，闸门对它们只是短路）。
/// 2. **bool 在数值之后**：sqlite 的 `bool::compatible` 连 `Int4 | Integer`
///    一起算兼容，MySQL 的含全部整数列类型（Tiny/Short/Long/Int24/LongLong/Bit），
///    两者的 decode 又都是「非 0 即真」（MySQL 还是 i8 解码，≥128 直接报错）。
///    bool 若排最前，任意整数（如 `SELECT 42`）都会变成 `true`。
/// 3. **整数/浮点分支要按驱动的实际类型补齐**：漏一个类型不是报错而是**静默 null**。
///    - PG 的 `smallint`(int2)/`real`(float4)：sqlx-postgres 的整数/浮点类型没有
///      `compatible` 覆写，走 OID 精确匹配（i64→INT8、i32→INT4、f64→FLOAT8），
///      所以要 i16 与 f32。
///    - MySQL 的 `* UNSIGNED`：`int_compatible` 带 `!UNSIGNED`
///      （`sqlx-mysql-0.8.6/src/types/int.rs:10`），所有有符号分支都拒绝，
///      只有 `u64`（`uint_compatible`，`src/types/uint.rs:17`）认。
///      `BIGINT UNSIGNED` 是最常见的自增主键类型，漏掉它 0/1 会变 `false`、
///      ≥128 会变 `null`。
/// 4. **日期只输出日期**：`time::Date` 没有时间分量，输出纯 `YYYY-MM-DD`
///    （`Display` 的实际形态见 `num_fmt::four_to_six_digits`：年份不足 4 位补零），
///    不能凭 `midnight()` 发明一个「UTC 午夜」时刻。附带好处：sqlite 的
///    `Date::compatible` 含 `Text`，形状像 `YYYY-MM-DD` 的文本列会被这一支接走，
///    输出纯日期后规范形文本**原样返回**，版本号/业务编码这类不透明文本不被改写。
///
/// 链尾**报错而非 null**：真 NULL 已被上面的闸门拦下，能走到链尾的只有
/// 「类型不在链上」（PG 的 numeric/uuid/jsonb、MySQL 的 decimal…）。Any 驱动
/// 时代这些列在 fetch 阶段就整条查询失败，原生驱动会安静地送到这里；若返回
/// `Null`，不支持的列会变成静默 null 一路进 ORM 实体，而本库按设计不提供
/// CAST 绕过口。
macro_rules! cell_fn {
    ($name:ident, $row:ty) => {
        cell_fn!(@chain $name, $row);
    };
    ($name:ident, $row:ty, unsigned) => {
        cell_fn!(@chain $name, $row, u64);
    };
    (@chain $name:ident, $row:ty $(, $unsigned:ty)?) => {
        pub fn $name(row: &$row, col: &str) -> Result<serde_json::Value, RdbmsError> {
            if row.try_get_raw(col).is_ok_and(|v| v.is_null()) {
                return Ok(serde_json::Value::Null);
            }
            if let Ok(n) = row.try_get::<i64, _>(col) {
                return Ok(serde_json::Value::Number(n.into()));
            }
            if let Ok(n) = row.try_get::<i32, _>(col) {
                return Ok(serde_json::Value::Number((n as i64).into()));
            }
            // PG 的 int2
            if let Ok(n) = row.try_get::<i16, _>(col) {
                return Ok(serde_json::Value::Number((n as i64).into()));
            }
            if let Ok(n) = row.try_get::<f64, _>(col) {
                return Ok(float_to_json(n));
            }
            // PG 的 real/float4
            if let Ok(n) = row.try_get::<f32, _>(col) {
                return Ok(f32_to_json(n));
            }
            $(
                // MySQL 的 UNSIGNED 整数列（含 BIGINT UNSIGNED 主键）：
                // 有符号分支全被 `!UNSIGNED` 拒绝，只有这里认。
                if let Ok(n) = row.try_get::<$unsigned, _>(col) {
                    return Ok(serde_json::Value::Number(n.into()));
                }
            )?
            if let Ok(b) = row.try_get::<bool, _>(col) {
                return Ok(serde_json::Value::Bool(b));
            }
            // PG 的 timestamptz、MySQL 的 datetime/timestamp
            if let Ok(dt) = row.try_get::<time::OffsetDateTime, _>(col) {
                return Ok(serde_json::Value::String(rfc3339(dt, col)?));
            }
            // PG 的 timestamp（无时区）；sqlite 的 datetime 文本
            if let Ok(dt) = row.try_get::<time::PrimitiveDateTime, _>(col) {
                return Ok(serde_json::Value::String(rfc3339(dt.assume_utc(), col)?));
            }
            // PG/MySQL 的 date；sqlite 的纯日期文本（上面的 datetime 解析要求带时间，先失败）
            if let Ok(d) = row.try_get::<time::Date, _>(col) {
                return Ok(serde_json::Value::String(d.to_string()));
            }
            if let Ok(s) = row.try_get::<String, _>(col) {
                return Ok(serde_json::Value::String(s));
            }
            if let Ok(b) = row.try_get::<Vec<u8>, _>(col) {
                return Ok(serde_json::Value::String(
                    base64::engine::general_purpose::STANDARD.encode(b),
                ));
            }
            Err(RdbmsError::Database(format!(
                "unsupported column type in result set: {col} ({})",
                row.try_get_raw(col)
                    .map(|v| v.type_info().name().to_string())
                    .unwrap_or_else(|_| "unknown".into())
            )))
        }
    };
}

cell_fn!(pg_cell_to_json, sqlx::postgres::PgRow);
// PG 不传 `unsigned`：PG 没有无符号整数类型，sqlx-postgres 更没实现 `u64`（编译不过）。
cell_fn!(mysql_cell_to_json, sqlx::mysql::MySqlRow, unsigned);
// sqlite 也不传：SQLite 的整数恒为 i64（i64 分支先接住），没有 UNSIGNED 语义。
cell_fn!(sqlite_cell_to_json, sqlx::sqlite::SqliteRow);

/// 行转换：任一列失败即整条查询失败（列名与类型名在 `cell_fn` 的错误信息里）。
macro_rules! rows_fn {
    ($name:ident, $row:ty, $cell:ident) => {
        pub fn $name(rows: Vec<$row>) -> Result<Vec<Row>, RdbmsError> {
            if rows.is_empty() {
                return Ok(Vec::new());
            }
            let columns: Vec<String> = rows[0]
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect();
            rows.iter()
                .map(|row| {
                    let values = columns
                        .iter()
                        .map(|col| $cell(row, col))
                        .collect::<Result<Vec<serde_json::Value>, RdbmsError>>()?;
                    Ok(Row::new(columns.clone(), values))
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
    use super::{f32_to_json, rfc3339};
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

    /// 纯日期文本走 `Date` 分支（`PrimitiveDateTime` 的解析要求「日期+时间」，
    /// 对 `2026-10-05` 会先失败），产出**纯日期**。
    ///
    /// sqlite 的 `Date::compatible` 含 `Text`，所以这一支会接走形状像日期的文本列；
    /// 输出纯日期后规范形文本原样返回 —— 这正是要保的性质（版本号、业务编码
    /// 这类不透明文本不能被补上时刻和时区）。
    #[tokio::test]
    async fn date_shaped_text_round_trips_as_plain_date() {
        let db = mem_sqlite("cell_date").await;
        let rows = db.query("SELECT '2026-10-05' AS d").await.unwrap();
        assert_eq!(
            rows[0].get("d"),
            Some(&serde_json::json!("2026-10-05")),
            "Date 分支未生效或不是纯日期"
        );
    }

    /// `f32` 走最短往返表示：直接 `as f64` 会得到 `0.10000000149011612`。
    #[test]
    fn f32_uses_shortest_round_trip_representation() {
        assert_eq!(f32_to_json(0.1f32), serde_json::json!(0.1));
        assert_eq!(f32_to_json(1.5f32), serde_json::json!(1.5));
        // NaN/±Inf 的 Display 解析不回数字，仍按字符串表示
        assert_eq!(f32_to_json(f32::NAN), serde_json::json!("NaN"));
        assert_eq!(f32_to_json(f32::INFINITY), serde_json::json!("Infinity"));
    }

    /// 年份超出 RFC3339 的 `0000-9999` 必须报错，而不是溜出一个格式不对的字符串。
    #[test]
    fn rfc3339_errors_on_out_of_range_year() {
        // 公元前 1 年 = time 的 year 0；再往前是负数年份，RFC3339 表达不了。
        let d = time::Date::from_calendar_date(-1, time::Month::January, 1).unwrap();
        let msg = rfc3339(d.midnight().assume_utc(), "born_at")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("born_at") && msg.contains("RFC3339"), "{msg}");
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

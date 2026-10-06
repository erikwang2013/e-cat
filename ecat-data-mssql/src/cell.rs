// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! `ColumnData` → `serde_json::Value` 的行转换。
//!
//! 与 sqlx 路径（`ecat-data-sqlx/src/cell.rs`）是**同一套输出契约**，但结构不同：
//! `ColumnData` 是**带标签的枚举**，取值时类型已知，所以这里是**单个 `match`**，
//! 不是那边「按顺序试探类型」的链 —— 批次 1 在链上踩过的两个 Critical
//! （`bool` 抢整数、`u64` 被漏）在这里**结构性地不存在**；NULL 天然是每个变体的
//! `Some`/`None`，也不需要提前的 NULL 闸门。
//!
//! 剩下的唯一一类问题是明确的「这个变体不支持」，且**报错而不是返回 null** ——
//! 静默 null 会把「不在支持范围」变成无声的数据缺失（与 sqlx 路径的链尾同一条理由）。

use base64::Engine as _;
use ecat_data::RdbmsError;
use serde_json::Value;
use tiberius::{ColumnData, FromSql};

/// 浮点转 JSON：`serde_json::Number` 装不下 NaN/±Inf，转字符串
/// （与 sqlx 路径的 `float_to_json` 同一条既有约定）。
fn float_to_json(n: f64) -> Value {
    if n.is_finite() {
        serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
    } else if n.is_nan() {
        Value::String("NaN".into())
    } else if n > 0.0 {
        Value::String("Infinity".into())
    } else {
        Value::String("-Infinity".into())
    }
}

/// `f32` → JSON：先过一趟 f32 的**最短往返十进制**表示再解析回 f64。
///
/// f32 的 `Display` 用最短往返算法（`0.1f32.to_string()` 得 `"0.1"`）；直接
/// `n as f64` 会把 `0.1` 加宽成 `0.10000000149011612` —— 数值上没错，但那是二进制
/// 加宽的原样值，不是这个 f32 的语义。`NaN`/`±Inf` 的 `Display` 解析不回数字，
/// 回落到 [`float_to_json`] 的字符串表示（与 sqlx 路径逐字同款）。
fn f32_to_json(n: f32) -> Value {
    match n.to_string().parse::<f64>() {
        Ok(v) => float_to_json(v),
        Err(_) => float_to_json(n as f64),
    }
}

/// 时间戳统一**归一化到 UTC** 再按 RFC3339 输出。
///
/// `time::OffsetDateTime` 的 `Display` **不是** RFC3339（形如
/// `2026-10-05 12:34:56.0 +00:00:00`，带偏移量且小时不补零），必须显式
/// `format(&Rfc3339)`；ORM 侧按 RFC3339 解析。
///
/// 格式化失败**报错**（带列名）而不是回落 `to_string()`：那样只会溜出一个格式
/// 不对的字符串喂给按 RFC3339 解析的调用方。超范围年份（RFC3339 只认 `0000-9999`）
/// 就该在这里响亮地失败。与 sqlx 路径逐字同款。
fn rfc3339(dt: time::OffsetDateTime, col: &str) -> Result<String, RdbmsError> {
    dt.to_offset(time::UtcOffset::UTC)
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| {
            RdbmsError::Database(format!(
                "failed to format datetime column {col} as RFC3339: {e}"
            ))
        })
}

/// 纯时刻输出 `HH:MM:SS[.fff…]`（小数位裁掉尾零）。
///
/// **不用 `time::Time` 的 `Display`**：它是 `0:00:00.0` 这种形态 —— 小时不补零
/// （`time-0.3.54/src/time.rs:1015` 用 `one_to_two_digits_no_padding`），小数位至少
/// 一位（`src/num_fmt.rs:426` 的 `truncated_subsecond_from_nanos` 返回 1..=9 位）。
/// 于是同一列里 `0:00:00.0` 解析不了而 `12:34:56.0` 能解析（ISO 8601/RFC3339 要求
/// 小时两位）—— 值的可解析性取决于它是几点，不能接受。
///
/// 这里给 SQL Server 自己的文本形态：补零到 `HH:MM:SS`，有小数才带小数、且不补
/// 到固定位数（不凭空造精度，也不丢精度 —— scale 7 的 7 位小数原样输出）。
fn time_of_day(t: time::Time) -> String {
    let mut s = format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second());
    let nanos = t.nanosecond();
    if nanos != 0 {
        s.push('.');
        s.push_str(format!("{nanos:09}").trim_end_matches('0'));
    }
    s
}

/// 变体已确定是日期/时间时，借驱动自带的 `FromSql` 换算成 `time` crate 的类型。
///
/// 不自己算：1900 基准日、1/300 秒碎片、`datetimeoffset` 的分钟偏移这些细节由
/// tiberius 维护（`src/tds/time/time.rs` 的 `from_sql!`）且带测试，重写一遍就是
/// 把驱动已经测过的换算复制成第二份真相。
fn time_value<T, F>(data: &ColumnData<'static>, col: &str, f: F) -> Result<Value, RdbmsError>
where
    // `from_sql` 的借用生命周期就是这次调用的生命周期，用 `for<'a>` 表达。
    for<'a> T: FromSql<'a>,
    F: FnOnce(T) -> Result<Value, RdbmsError>,
{
    match T::from_sql(data) {
        // 变体的 `None` 就是 SQL NULL。
        Ok(None) => Ok(Value::Null),
        Ok(Some(v)) => f(v),
        // 变体已经对上，正常走不到这里；真到了就是本函数的匹配与驱动的映射分叉了。
        Err(e) => Err(RdbmsError::Database(format!(
            "列 {col} 的日期时间解码失败: {e}"
        ))),
    }
}

/// `ColumnData` → `serde_json::Value`（一列的值）。
///
/// `match` **不设兜底分支**：18 个变体逐个列出，tiberius 新增变体时这里会**编译
/// 失败**而不是静默走进某个错误分支。
///
/// 两处取值约定与 sqlx 路径一致（spec §6）：
/// - 时间戳（`DateTime`/`SmallDateTime`/`DateTime2`/`DateTimeOffset`）→ **RFC3339 UTC**；
///   无时区的 `DateTime`/`SmallDateTime`/`DateTime2` 按既有约定当作 UTC
///   （与 sqlx 路径的 `PrimitiveDateTime` 同）。
/// - `Date` → **纯 `YYYY-MM-DD`**，`Time` → **纯时刻**：源数据里没有的时刻/时区
///   不补（`2026-10-05` 补成 `2026-10-05T00:00:00Z` 是凭空断言）。
///
/// `Numeric`（Decimal 的表示形态要等 ORM 定）与 `Xml` **报错而非 null** —— 报错
/// 保证批次 3 写映射时一定撞上。这两类的 SQL NULL 仍然是 NULL：NULL 是「没有值」，
/// 不是「表示形态未定」，与 sqlx 路径先过 NULL 闸门同。
pub fn cell_to_json(data: &ColumnData<'static>, col: &str) -> Result<Value, RdbmsError> {
    use ColumnData as C;

    match data {
        C::U8(v) => Ok(v.map_or(Value::Null, |n| Value::Number(n.into()))),
        C::I16(v) => Ok(v.map_or(Value::Null, |n| Value::Number(n.into()))),
        C::I32(v) => Ok(v.map_or(Value::Null, |n| Value::Number(n.into()))),
        C::I64(v) => Ok(v.map_or(Value::Null, |n| Value::Number(n.into()))),
        C::F32(v) => Ok(v.map_or(Value::Null, f32_to_json)),
        C::F64(v) => Ok(v.map_or(Value::Null, float_to_json)),
        C::Bit(v) => Ok(v.map_or(Value::Null, Value::Bool)),
        C::String(v) => Ok(v
            .as_deref()
            .map_or(Value::Null, |s| Value::String(s.into()))),
        // `Uuid` 的 `Display` 就是标准带连字符的小写形式。
        C::Guid(v) => Ok(v.map_or(Value::Null, |g| Value::String(g.to_string()))),
        // base64：与 sqlx 路径同一约定（`X'0102FF'` → `"AQL/"`）。
        C::Binary(v) => Ok(v.as_deref().map_or(Value::Null, |b| {
            Value::String(base64::engine::general_purpose::STANDARD.encode(b))
        })),
        C::DateTime(_) | C::SmallDateTime(_) | C::DateTime2(_) => {
            time_value::<time::PrimitiveDateTime, _>(data, col, |dt| {
                Ok(Value::String(rfc3339(dt.assume_utc(), col)?))
            })
        }
        C::DateTimeOffset(_) => time_value::<time::OffsetDateTime, _>(data, col, |dt| {
            Ok(Value::String(rfc3339(dt, col)?))
        }),
        // `time::Date` 的 `Display` 就是 `YYYY-MM-DD`（`time-0.3.54/src/date.rs` 的
        // `fmt_into_buffer`：四位补零年份 + `-` + 两位月 + `-` + 两位日；符号位只在
        // 负年份/超四位年份时占位）。
        C::Date(_) => time_value::<time::Date, _>(data, col, |d| Ok(Value::String(d.to_string()))),
        C::Time(_) => time_value::<time::Time, _>(data, col, |t| Ok(Value::String(time_of_day(t)))),
        C::Numeric(None) => Ok(Value::Null),
        C::Numeric(Some(_)) => Err(RdbmsError::Database(format!(
            "unsupported column type in result set: {col} (NUMERIC)"
        ))),
        C::Xml(None) => Ok(Value::Null),
        C::Xml(Some(_)) => Err(RdbmsError::Database(format!(
            "unsupported column type in result set: {col} (XML)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::cell_to_json;
    use serde_json::json;
    use std::borrow::Cow;
    // 驱动侧的 TDS 时间类型（不是 `time` crate 的那几个同名类型）。
    use tiberius::ColumnData;
    use tiberius::time as tds;

    // 天数用**已知常量**而不是在测试里现算 —— 现算等于拿同一套算法去断言自己。
    /// TDS `date` 的天数基准是 0001-01-01（SQL Server 的 `date` 纪元）。
    const DAYS_TO_2026_10_05: u32 = 739_893;
    /// TDS `datetime`/`smalldatetime` 的天数基准是 1900-01-01。
    const DAYS_1900_TO_2026_10_05: u32 = 46_298;

    fn date_2026_10_05() -> tds::Date {
        tds::Date::new(DAYS_TO_2026_10_05)
    }

    /// `Time`/`DateTime2` 的增量单位是 10^-scale 秒，驱动写入用 scale 7。
    fn increments(h: u64, m: u64, s: u64) -> u64 {
        (h * 3600 + m * 60 + s) * 10_000_000
    }

    fn time_12_34_56() -> tds::Time {
        tds::Time::new(increments(12, 34, 56), 7)
    }

    fn datetime2_2026_10_05() -> tds::DateTime2 {
        tds::DateTime2::new(date_2026_10_05(), time_12_34_56())
    }

    /// `date` 输出**纯日期**：源数据里没有时刻也没有时区，补成
    /// `2026-10-05T00:00:00Z` 是凭空断言（批次 1 定案，spec §6）。
    #[test]
    fn date_is_plain_yyyy_mm_dd_not_a_fabricated_instant() {
        assert_eq!(
            cell_to_json(&ColumnData::Date(Some(date_2026_10_05())), "d").unwrap(),
            json!("2026-10-05")
        );
        // 月/日/年都补零到两位/四位（`0001-01-01` 是 `date` 的最小值，天数 0）。
        assert_eq!(
            cell_to_json(&ColumnData::Date(Some(tds::Date::new(0))), "d").unwrap(),
            json!("0001-01-01")
        );
    }

    /// 时间戳一律 RFC3339 **UTC**：三种无时区类型都当 UTC（与 sqlx 路径的
    /// `PrimitiveDateTime` 同），UTC 输入也要带 `Z` 而不是裸时间。
    #[test]
    fn datetime_is_rfc3339_utc() {
        assert_eq!(
            cell_to_json(&ColumnData::DateTime2(Some(datetime2_2026_10_05())), "at").unwrap(),
            json!("2026-10-05T12:34:56Z")
        );

        // 旧式 `datetime`：天数是 1900 基准，秒是 1/300 秒碎片（45296 s × 300）。
        let dt = tds::DateTime::new(DAYS_1900_TO_2026_10_05 as i32, 45_296 * 300);
        assert_eq!(
            cell_to_json(&ColumnData::DateTime(Some(dt)), "at").unwrap(),
            json!("2026-10-05T12:34:56Z")
        );

        // `smalldatetime`：分钟精度（12:34:56 会落成 12:34:00）。
        let sdt = tds::SmallDateTime::new(DAYS_1900_TO_2026_10_05 as u16, 12 * 60 + 34);
        assert_eq!(
            cell_to_json(&ColumnData::SmallDateTime(Some(sdt)), "at").unwrap(),
            json!("2026-10-05T12:34:00Z")
        );
    }

    /// 带偏移的 `datetimeoffset` 归一化到 UTC 再输出。
    ///
    /// 构造的是 `DateTime2(2026-10-05 12:34:56)` + 偏移 **+480 分钟**。TDS 里
    /// datetime2 部分装的是 **UTC 时刻**，偏移只是显示用的时区 —— 所以这个输入在
    /// 驱动语义下是「12:34:56Z，显示为 +08:00 的 20:34:56」，UTC 呈现就是 12:34:56Z。
    ///
    /// 这一点的依据是驱动自己的真库集成测试（`tiberius-ng/tests/query.rs` 的
    /// `offset_date_time_fixed_with_time_crate_conversion`）：
    /// `CAST('2020-04-20T16:20:00+03:00' AS datetimeoffset(7))` 读回来等于
    /// `2020-04-20 16:20:00 +03:00` —— 只有 datetime2 存的是 UTC（13:20）时，
    /// `assume_utc(wall).to_offset(+03:00)` 才会得到这个瞬时。
    ///
    /// 断言钉住的是 `rfc3339` 里的 `to_offset(UTC)`：漏掉那一步会输出本地时刻带偏移
    /// 的 `"2026-10-05T20:34:56+08:00"`（RFC3339 走非零偏移分支），立刻失败。
    #[test]
    fn datetimeoffset_is_normalized_to_utc() {
        let dto = tds::DateTimeOffset::new(datetime2_2026_10_05(), 480);
        assert_eq!(
            cell_to_json(&ColumnData::DateTimeOffset(Some(dto)), "at").unwrap(),
            json!("2026-10-05T12:34:56Z")
        );
        // 零偏移的同一时刻给出同一个 UTC 串（两条路径是同一个瞬时）。
        let utc = tds::DateTimeOffset::new(datetime2_2026_10_05(), 0);
        assert_eq!(
            cell_to_json(&ColumnData::DateTimeOffset(Some(utc)), "at").unwrap(),
            json!("2026-10-05T12:34:56Z")
        );
    }

    /// `time` 列输出**纯时刻**（无日期分量）：补零到 `HH:MM:SS`，有小数才带小数。
    /// `Display` 给的是 `12:34:56.0` / `0:00:00.0`（小时不补零、小数位至少一位），
    /// 见 [`super::time_of_day`] 的理由。
    #[test]
    fn time_is_plain_time_without_date() {
        assert_eq!(
            cell_to_json(&ColumnData::Time(Some(time_12_34_56())), "t").unwrap(),
            json!("12:34:56")
        );
        // 午夜：小时补零（`Display` 会给解析不了的 `0:00:00.0`）。
        assert_eq!(
            cell_to_json(&ColumnData::Time(Some(tds::Time::new(0, 7))), "t").unwrap(),
            json!("00:00:00")
        );
        // 小数不丢：scale 7 的 1_234_500 增量 = 0.1234500 秒 → 尾零裁掉后是 0.12345。
        let t = tds::Time::new(increments(1, 2, 3) + 1_234_500, 7);
        assert_eq!(
            cell_to_json(&ColumnData::Time(Some(t)), "t").unwrap(),
            json!("01:02:03.12345")
        );
    }

    /// NULL 是每个变体的 `None`，不需要额外闸门；不支持的类型的 NULL 也还是 NULL
    /// （NULL 是「没有值」，不是「表示形态未定」—— 与 sqlx 路径先过 NULL 同）。
    /// 18 个变体一个不落：漏一个变体的 NULL 分支就是静默的 `Null` 或静默报错。
    #[test]
    fn null_is_null_for_every_variant() {
        for data in [
            ColumnData::U8(None),
            ColumnData::I16(None),
            ColumnData::I32(None),
            ColumnData::I64(None),
            ColumnData::F32(None),
            ColumnData::F64(None),
            ColumnData::Bit(None),
            ColumnData::String(None),
            ColumnData::Guid(None),
            ColumnData::Binary(None),
            ColumnData::Numeric(None),
            ColumnData::Xml(None),
            ColumnData::DateTime(None),
            ColumnData::SmallDateTime(None),
            ColumnData::Time(None),
            ColumnData::Date(None),
            ColumnData::DateTime2(None),
            ColumnData::DateTimeOffset(None),
        ] {
            assert!(cell_to_json(&data, "x").unwrap().is_null(), "got: {data:?}");
        }
    }

    /// `f32` 走最短往返表示：直接 `as f64` 会得到 `0.10000000149011612`。
    #[test]
    fn f32_uses_shortest_round_trip() {
        assert_eq!(
            cell_to_json(&ColumnData::F32(Some(0.1)), "f").unwrap(),
            json!(0.1)
        );
        assert_eq!(
            cell_to_json(&ColumnData::F32(Some(1.5)), "f").unwrap(),
            json!(1.5)
        );
    }

    /// `serde_json::Number` 装不下 NaN/±Inf → 字符串（与 sqlx 路径同约定）。
    #[test]
    fn f32_nan_and_inf_fall_back_to_strings() {
        assert_eq!(
            cell_to_json(&ColumnData::F32(Some(f32::NAN)), "f").unwrap(),
            json!("NaN")
        );
        assert_eq!(
            cell_to_json(&ColumnData::F32(Some(f32::INFINITY)), "f").unwrap(),
            json!("Infinity")
        );
        assert_eq!(
            cell_to_json(&ColumnData::F32(Some(f32::NEG_INFINITY)), "f").unwrap(),
            json!("-Infinity")
        );
        assert_eq!(
            cell_to_json(&ColumnData::F64(Some(f64::NAN)), "f").unwrap(),
            json!("NaN")
        );
    }

    /// base64：与 sqlx 路径同一约定（`X'0102FF'` → `"AQL/"`）。
    #[test]
    fn binary_is_base64() {
        let v = cell_to_json(
            &ColumnData::Binary(Some(Cow::Borrowed(&[1_u8, 2, 0xFF]))),
            "raw",
        )
        .unwrap();
        assert_eq!(v, json!("AQL/"));
    }

    /// `Guid` 是标准带连字符的小写 UUID 字符串。
    #[test]
    fn guid_is_hyphenated_string() {
        let g = tiberius::Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
        assert_eq!(
            cell_to_json(&ColumnData::Guid(Some(g)), "id").unwrap(),
            json!("67e55044-10b1-426f-9247-bb680e5fe0c8")
        );
    }

    /// 整数/字符串原样：整数是数字而不是 `true`（sqlx 链上 `bool` 抢整数那个
    /// Critical 在单 `match` 下结构性地不存在，这里把它钉住）。
    #[test]
    fn integers_stay_numbers_and_strings_stay_strings() {
        assert_eq!(
            cell_to_json(&ColumnData::I64(Some(42)), "n").unwrap(),
            json!(42)
        );
        assert_eq!(
            cell_to_json(&ColumnData::U8(Some(200)), "n").unwrap(),
            json!(200)
        );
        assert_eq!(
            cell_to_json(&ColumnData::Bit(Some(true)), "b").unwrap(),
            json!(true)
        );
        assert_eq!(
            cell_to_json(&ColumnData::String(Some(Cow::Borrowed("2026-10-05"))), "s").unwrap(),
            json!("2026-10-05")
        );
    }

    /// 链尾**报错而非 null**，消息带列名与类型名（与 sqlx 路径的链尾同格式）——
    /// Decimal 的表示形态要等 ORM 定，报错保证批次 3 写映射时一定撞上。
    #[test]
    fn numeric_errors_loudly_with_column_and_type() {
        let n = tiberius::numeric::Numeric::new_with_scale(12_345, 2);
        let msg = cell_to_json(&ColumnData::Numeric(Some(n)), "amount")
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("amount") && msg.contains("NUMERIC"),
            "got: {msg}"
        );
    }

    #[test]
    fn xml_errors_loudly_with_column_and_type() {
        let x = tiberius::xml::XmlData::new("<a/>");
        let msg = cell_to_json(&ColumnData::Xml(Some(Cow::Owned(x))), "doc")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("doc") && msg.contains("XML"), "got: {msg}");
    }
}

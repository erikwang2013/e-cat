// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `Row` 内的时间值 ↔ Rust 时间类型。契约按**语义**分两种形态，不是按驱动分
//! （spec §6 时间类型策略）：
//!
//! | 语义 | `Row` 内呈现 | Rust 类型 |
//! |---|---|---|
//! | 时间戳 | RFC3339 UTC（`2026-10-05T12:34:56Z`） | [`time::OffsetDateTime`] |
//! | 纯日期 | `YYYY-MM-DD`（`2026-10-05`） | [`time::Date`] |
//!
//! 两者都是 ISO 8601；RFC3339 是时间戳子集，纯日期不在其内。
//!
//! Task 5（`value.rs`）是这四个 helper 的首批消费者；在那之前本 crate 内无调用点，
//! 故暂时豁免 `dead_code`。**Task 5 落地后请删掉下面这行。**
#![allow(dead_code)]

use time::OffsetDateTime;
use time::UtcOffset;
use time::format_description::FormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

use crate::error::OrmError;

/// `YYYY-MM-DD`。解析与格式化共用同一描述符，避免两边补零规则不一致。
const DATE_FMT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

/// 转 RFC3339 **并归一化到 UTC**。
///
/// # Panics
///
/// 仅当年份落在 RFC3339 可表示的 `0000..=9999` 之外时 panic。四个受支持后端的
/// 日期范围都在此界内（MySQL `TIMESTAMP` 是 1970–2038；SQL Server `DATETIME2`
/// 是 0001–9999；SQLite/PG 实际使用同样远窄于 ±9999）。这个界写在签名里而不是
/// 悄悄取模或截断 —— 越界说明数据本身已经坏了。
pub fn to_rfc3339_utc(t: OffsetDateTime) -> String {
    t.to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .expect("year outside RFC3339's 0000..=9999 range")
}

/// 解析 RFC3339（接受带偏移量的输入，内部转 UTC）。
///
/// 报错信息里带上**原始输入**：只报「parse failed」无法定位是哪个值坏了。
pub fn from_rfc3339(s: &str) -> Result<OffsetDateTime, OrmError> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map(|t| t.to_offset(UtcOffset::UTC))
        .map_err(|e| {
            OrmError::Rdbms(ecat_data::RdbmsError::Database(format!(
                "invalid RFC3339 timestamp `{s}`: {e}"
            )))
        })
}

/// `Date` → `YYYY-MM-DD`。**不补时刻、不补时区。**
pub fn to_date_string(d: time::Date) -> String {
    d.format(DATE_FMT)
        .expect("DATE_FMT is a valid format description")
}

/// `YYYY-MM-DD` → `Date`。
pub fn from_date_string(s: &str) -> Result<time::Date, OrmError> {
    time::Date::parse(s, DATE_FMT).map_err(|e| {
        OrmError::Rdbms(ecat_data::RdbmsError::Database(format!(
            "invalid date `{s}` (expected YYYY-MM-DD): {e}"
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn timestamp_is_normalized_to_utc() {
        // +08:00 的 12:00 == UTC 的 04:00。**写库前必须转 UTC**，
        // 否则不同偏移写入的同一时刻在文本比较下不相等（spec:529-530）。
        let t = datetime!(2026-10-05 12:00:00 +8);
        assert_eq!(to_rfc3339_utc(t), "2026-10-05T04:00:00Z");
    }

    #[test]
    fn rfc3339_roundtrip_preserves_instant() {
        let t = datetime!(2026-10-05 04:00:00 UTC);
        let s = to_rfc3339_utc(t);
        assert_eq!(from_rfc3339(&s).unwrap(), t);
    }

    #[test]
    fn from_rfc3339_accepts_offset_and_converts() {
        let t = from_rfc3339("2026-10-05T12:00:00+08:00").unwrap();
        assert_eq!(t, datetime!(2026-10-05 04:00:00 UTC));
    }

    /// 纯日期**不补时刻、不补时区** —— 源数据里没有这些信息（spec:638-642）。
    #[test]
    fn date_is_plain_without_time_or_zone() {
        let d = time::macros::date!(2026 - 10 - 05);
        assert_eq!(to_date_string(d), "2026-10-05");
        assert!(!to_date_string(d).contains('T'));
        assert!(!to_date_string(d).contains('Z'));
    }

    #[test]
    fn date_roundtrip() {
        let d = time::macros::date!(2026 - 10 - 05);
        assert_eq!(from_date_string(&to_date_string(d)).unwrap(), d);
    }

    /// 回归：`OffsetDateTime` 的 `Display` **不是** RFC3339（批次 1 踩过）。
    /// 这个测试钉死我们走的是 `.format(&Rfc3339)` 而不是 `to_string()`。
    #[test]
    fn output_is_not_the_display_impl() {
        let t = datetime!(2026-10-05 04:00:00 UTC);
        assert_ne!(to_rfc3339_utc(t), t.to_string());
        assert!(to_rfc3339_utc(t).ends_with('Z'));
    }

    #[test]
    fn malformed_input_errors_instead_of_guessing() {
        assert!(from_rfc3339("not a date").is_err());
        assert!(from_date_string("2026-10-05T00:00:00Z").is_err()); // 日期列不该拿到时间戳
        assert!(from_date_string("").is_err());
    }
}

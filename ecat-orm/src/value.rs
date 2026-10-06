// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 实体字段 ↔ `serde_json::Value`。
//!
//! **不要**用 `serde_json::to_value(&field)` 代替本模块：`time` 的 serde 实现会把
//! `OffsetDateTime` 序列化成 `[year, ordinal, hour, minute, second, nanosecond, offset]`
//! 数组，`Vec<u8>` 会变成数字数组 —— 两者都不是 `Row` 的线格式。

use base64::Engine as _;
use ecat_data::Row;
use serde_json::Value;
use time::OffsetDateTime;

use crate::entity::ColType;
use crate::error::OrmError;
use crate::time::{from_date_string, from_rfc3339, to_date_string, to_rfc3339_utc};

/// 一个能作为实体列存储的 Rust 类型。
pub trait ColumnValue: Sized {
    /// 对应的存储类型，供迁移 DDL 使用。
    const COL_TYPE: ColType;

    /// 该类型能否接收 NULL。只有 `Option<T>` 为 `true`。
    ///
    /// [`from_row_col`] 靠它区分「列为 NULL」（→ [`OrmError::UnexpectedNull`]）
    /// 与「值类型不符」（→ [`OrmError::TypeMismatch`]）—— 没有这个标记时两者
    /// 都只能落到 `from_json` 里，NULL 会被当成类型不符报出去。
    const NULLABLE: bool = false;

    fn to_json(&self) -> Value;

    /// `v` 保证不是 `Value::Null`（调用方已拦掉）。
    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError>;
}

impl<T: ColumnValue> ColumnValue for Option<T> {
    const COL_TYPE: ColType = T::COL_TYPE;
    const NULLABLE: bool = true;

    fn to_json(&self) -> Value {
        match self {
            None => Value::Null,
            Some(v) => v.to_json(),
        }
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        if v.is_null() {
            return Ok(None);
        }
        T::from_json(column, v).map(Some)
    }
}

/// 从一行里取一列并转换。
///
/// **三种情形必须可区分**（这是本函数的全部存在理由）：
/// - 列不在结果集里 → [`OrmError::UnknownColumn`]（查询写错了，不是数据问题）
/// - 列在但为 NULL，字段非 `Option` → [`OrmError::UnexpectedNull`]
/// - 列在但为 NULL，字段是 `Option<T>` → `Ok(None)`
pub fn from_row_col<T: ColumnValue>(row: &Row, column: &'static str) -> Result<T, OrmError> {
    match row.get(column) {
        None => Err(OrmError::UnknownColumn(column.into())),
        Some(v) if v.is_null() && !T::NULLABLE => Err(OrmError::UnexpectedNull { column }),
        Some(v) => T::from_json(column, v),
    }
}

/// 类型不符时的统一报错。带上列名与期望类型 —— 否则用户只看到
/// 「expected i64」却不知道是哪一列。
fn mismatch(column: &'static str, expected: &'static str) -> OrmError {
    OrmError::TypeMismatch { column, expected }
}

macro_rules! int_col {
    ($t:ty, $ct:expr, $expected:expr) => {
        impl ColumnValue for $t {
            const COL_TYPE: ColType = $ct;
            fn to_json(&self) -> Value {
                Value::from(*self)
            }
            fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
                // i32 也接受 Number 里的 i64（SQLite 只存 i64），越界才报错 ——
                // 直接 as 截断会把 70000 静默变成 4464。
                v.as_i64()
                    .and_then(|n| <$t>::try_from(n).ok())
                    .ok_or_else(|| mismatch(column, $expected))
            }
        }
    };
}

int_col!(i64, ColType::I64, "i64");
int_col!(i32, ColType::I32, "i32");

impl ColumnValue for f64 {
    const COL_TYPE: ColType = ColType::F64;

    fn to_json(&self) -> Value {
        if self.is_finite() {
            serde_json::Number::from_f64(*self).map_or(Value::Null, Value::Number)
        } else if self.is_nan() {
            Value::String("NaN".into())
        } else if *self > 0.0 {
            Value::String("inf".into())
        } else {
            Value::String("-inf".into())
        }
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        // 与 to_json 对称：字符串形态的 NaN/±Inf 要能转回来。
        // 两个驱动实际写出的是 `Infinity` / `-Infinity`
        // （`ecat-data-sqlx/src/cell.rs:15-17`），也一并接受 ——
        // 否则读驱动产出的行会假报 TypeMismatch。
        match v {
            Value::Number(n) => n.as_f64().ok_or_else(|| mismatch(column, "f64")),
            Value::String(s) => match s.as_str() {
                "NaN" => Ok(f64::NAN),
                "inf" | "Infinity" => Ok(f64::INFINITY),
                "-inf" | "-Infinity" => Ok(f64::NEG_INFINITY),
                _ => Err(mismatch(column, "f64")),
            },
            _ => Err(mismatch(column, "f64")),
        }
    }
}

impl ColumnValue for bool {
    const COL_TYPE: ColType = ColType::Bool;

    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        // 后端对布尔的呈现不一致（SQLite/MySQL 是 1/0，PG 是真布尔，
        // SQL Server 是 BIT=1/0），两种都收。
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Number(n) => n
                .as_i64()
                .map(|i| i != 0)
                .ok_or_else(|| mismatch(column, "bool")),
            _ => Err(mismatch(column, "bool")),
        }
    }
}

impl ColumnValue for String {
    const COL_TYPE: ColType = ColType::Text;

    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| mismatch(column, "string"))
    }
}

impl ColumnValue for Vec<u8> {
    const COL_TYPE: ColType = ColType::Bytes;

    fn to_json(&self) -> Value {
        Value::String(base64::engine::general_purpose::STANDARD.encode(self))
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .ok_or_else(|| mismatch(column, "base64 bytes"))
    }
}

impl ColumnValue for OffsetDateTime {
    const COL_TYPE: ColType = ColType::Timestamp;

    fn to_json(&self) -> Value {
        Value::String(to_rfc3339_utc(*self))
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .ok_or_else(|| mismatch(column, "RFC3339 timestamp string"))
            .and_then(from_rfc3339)
    }
}

impl ColumnValue for time::Date {
    const COL_TYPE: ColType = ColType::Date;

    fn to_json(&self) -> Value {
        Value::String(to_date_string(*self))
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .ok_or_else(|| mismatch(column, "YYYY-MM-DD date string"))
            .and_then(from_date_string)
    }
}

impl ColumnValue for Value {
    const COL_TYPE: ColType = ColType::Json;

    fn to_json(&self) -> Value {
        self.clone()
    }

    fn from_json(_column: &'static str, v: &Value) -> Result<Self, OrmError> {
        Ok(v.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_data::Row;
    use serde_json::json;

    fn row(pairs: Vec<(&str, serde_json::Value)>) -> Row {
        let (cols, vals) = pairs.into_iter().map(|(c, v)| (c.to_string(), v)).unzip();
        Row::new(cols, vals)
    }

    #[test]
    fn integers_roundtrip() {
        let r = row(vec![("n", json!(42))]);
        assert_eq!(from_row_col::<i64>(&r, "n").unwrap(), 42);
    }

    #[test]
    fn missing_column_is_an_error_not_a_default() {
        // 列不在结果集里 ≠ 列为 NULL。前者是查询写错了，必须响亮报错。
        let r = row(vec![("other", json!(1))]);
        let e = from_row_col::<i64>(&r, "n").unwrap_err();
        assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
    }

    #[test]
    fn null_into_non_optional_is_an_error() {
        let r = row(vec![("n", json!(null))]);
        let e = from_row_col::<i64>(&r, "n").unwrap_err();
        assert!(
            matches!(e, OrmError::UnexpectedNull { column: "n" }),
            "got: {e:?}"
        );
    }

    #[test]
    fn null_into_optional_is_none() {
        let r = row(vec![("n", json!(null))]);
        assert_eq!(from_row_col::<Option<i64>>(&r, "n").unwrap(), None);
    }

    #[test]
    fn optional_some_roundtrips() {
        let r = row(vec![("n", json!(7))]);
        assert_eq!(from_row_col::<Option<i64>>(&r, "n").unwrap(), Some(7));
    }

    /// 时间戳必须是 RFC3339 字符串，不是 `time` crate 的 serde 数组形态。
    #[test]
    fn timestamp_uses_rfc3339_string_not_serde_array() {
        let t = time::macros::datetime!(2026-10-05 04:00:00 UTC);
        assert_eq!(t.to_json(), json!("2026-10-05T04:00:00Z"));
    }

    #[test]
    fn date_uses_plain_string() {
        let d = time::macros::date!(2026 - 10 - 05);
        assert_eq!(d.to_json(), json!("2026-10-05"));
    }

    #[test]
    fn bytes_use_base64_standard() {
        assert_eq!(b"\xff\xfe".to_vec().to_json(), json!("//4="));
    }

    #[test]
    fn bad_type_reports_column_name() {
        let r = row(vec![("n", json!("not a number"))]);
        let e = from_row_col::<i64>(&r, "n").unwrap_err();
        assert!(
            matches!(e, OrmError::TypeMismatch { column: "n", .. }),
            "got: {e:?}"
        );
    }

    /// 浮点的 JSON 表示沿用批次 1 的约定：NaN/±Inf 装不进 serde_json::Number，
    /// 转字符串而不是静默变 null（`ecat-data-sqlx/src/cell.rs:9-12` 同款处理）。
    #[test]
    fn non_finite_floats_become_strings() {
        assert_eq!(f64::NAN.to_json(), json!("NaN"));
        assert_eq!(f64::INFINITY.to_json(), json!("inf"));
        assert_eq!(1.5_f64.to_json(), json!(1.5));
    }

    /// 两个驱动写出的拼写是 `Infinity` / `-Infinity`（不是本模块 to_json 的
    /// `inf` / `-inf`）。`from_row_col` 的主用途正是读驱动产出的行，
    /// 两种拼写都必须认，否则合法数据被假报 TypeMismatch。
    #[test]
    fn driver_spelled_infinities_are_accepted() {
        let r = row(vec![("n", json!("Infinity")), ("m", json!("-Infinity"))]);
        assert_eq!(from_row_col::<f64>(&r, "n").unwrap(), f64::INFINITY);
        assert_eq!(from_row_col::<f64>(&r, "m").unwrap(), f64::NEG_INFINITY);
    }
}

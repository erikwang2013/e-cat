// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Conversions between CQL values and JSON.
//!
//! Result values use human-readable JSON where CQL's binary forms have an
//! obvious text form (UUIDs, timestamps, decimals); blobs become byte arrays,
//! matching how parameters are accepted.

use ecat_data::RdbmsError;
use scylla::value::{CqlDate, CqlDecimal, CqlTime, CqlTimestamp, CqlValue};
use serde_json::{Map, Value, json};

/// NULL stays `null`; everything else goes through [`value_to_json`].
pub(crate) fn cql_to_json(value: &Option<CqlValue>) -> Value {
    match value {
        None => Value::Null,
        Some(value) => value_to_json(value),
    }
}

fn value_to_json(value: &CqlValue) -> Value {
    match value {
        CqlValue::Ascii(text) | CqlValue::Text(text) => Value::String(text.clone()),
        CqlValue::Boolean(boolean) => Value::Bool(*boolean),
        CqlValue::TinyInt(int) => json!(int),
        CqlValue::SmallInt(int) => json!(int),
        CqlValue::Int(int) => json!(int),
        CqlValue::BigInt(int) => json!(int),
        CqlValue::Counter(counter) => json!(counter.0),
        CqlValue::Float(float) => json!(float),
        CqlValue::Double(double) => json!(double),
        CqlValue::Blob(bytes) => Value::Array(bytes.iter().map(|byte| json!(byte)).collect()),
        CqlValue::Uuid(uuid) => Value::String(uuid.to_string()),
        CqlValue::Timeuuid(uuid) => Value::String(uuid.to_string()),
        CqlValue::Timestamp(CqlTimestamp(millis)) => Value::String(format_timestamp(*millis)),
        CqlValue::Date(CqlDate(days)) => Value::String(format_date(*days)),
        CqlValue::Time(CqlTime(nanos)) => Value::String(format_time(*nanos)),
        CqlValue::Decimal(decimal) => Value::String(format_decimal(decimal)),
        CqlValue::Inet(address) => Value::String(address.to_string()),
        CqlValue::Varint(varint) => {
            Value::String(signed_be_to_decimal(varint.as_signed_bytes_be_slice()))
        }
        CqlValue::Duration(duration) => Value::String(format!(
            "{}mo{}d{}ns",
            duration.months, duration.days, duration.nanoseconds
        )),
        CqlValue::List(items) | CqlValue::Set(items) | CqlValue::Vector(items) => {
            Value::Array(items.iter().map(value_to_json).collect())
        }
        CqlValue::Map(entries) => {
            let mut map = Map::with_capacity(entries.len());
            for (key, value) in entries {
                // JSON object keys are strings; non-string CQL keys use their
                // JSON rendering (e.g. int 7 -> "7").
                let key = match value_to_json(key) {
                    Value::String(text) => text,
                    other => other.to_string(),
                };
                map.insert(key, value_to_json(value));
            }
            Value::Object(map)
        }
        CqlValue::Tuple(items) => Value::Array(
            items
                .iter()
                .map(|item| item.as_ref().map_or(Value::Null, value_to_json))
                .collect(),
        ),
        CqlValue::UserDefinedType { fields, .. } => {
            let mut map = Map::with_capacity(fields.len());
            for (name, value) in fields {
                map.insert(
                    name.clone(),
                    value.as_ref().map_or(Value::Null, value_to_json),
                );
            }
            Value::Object(map)
        }
        // `CqlValue` is `#[non_exhaustive]`: new native types must not panic.
        other => Value::String(format!("{other:?}")),
    }
}

/// Map one JSON parameter to a CQL value (`None` = CQL null).
///
/// Integers bind as `bigint` and floats as `double`; CQL markers are typed by
/// the server, so a column of another numeric width needs an explicit cast in
/// the statement.
pub(crate) fn json_to_cql(value: &Value) -> Result<Option<CqlValue>, RdbmsError> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(boolean) => Ok(Some(CqlValue::Boolean(*boolean))),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                Ok(Some(CqlValue::BigInt(int)))
            } else if number.as_u64().is_some() {
                Err(unsupported("integer exceeds the CQL bigint range"))
            } else {
                match number.as_f64() {
                    Some(float) => Ok(Some(CqlValue::Double(float))),
                    None => Err(unsupported("number is not representable as f64")),
                }
            }
        }
        Value::String(text) => Ok(Some(CqlValue::Text(text.clone()))),
        Value::Array(items) => {
            let mut bytes = Vec::with_capacity(items.len());
            for item in items {
                let byte = item
                    .as_u64()
                    .filter(|byte| *byte <= u64::from(u8::MAX))
                    .ok_or_else(|| {
                        unsupported("array parameters are blobs: expected integers 0..=255")
                    })?;
                bytes.push(byte as u8);
            }
            Ok(Some(CqlValue::Blob(bytes)))
        }
        Value::Object(_) => Err(unsupported("object parameters have no CQL representation")),
    }
}

/// Convert all parameters of a `*_with` call.
pub(crate) fn json_params(params: &[Value]) -> Result<Vec<Option<CqlValue>>, RdbmsError> {
    params.iter().map(json_to_cql).collect()
}

fn unsupported(message: &str) -> RdbmsError {
    RdbmsError::Database(format!("cassandra: unsupported parameter: {message}"))
}

/// Two's-complement big-endian bytes to a decimal string (decimal and varint).
fn signed_be_to_decimal(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "0".to_owned();
    }
    let negative = bytes[0] & 0x80 != 0;
    let mut magnitude = bytes.to_vec();
    if negative {
        for byte in &mut magnitude {
            *byte = !*byte;
        }
        let mut carry = 1u16;
        for byte in magnitude.iter_mut().rev() {
            let sum = u16::from(*byte) + carry;
            *byte = sum as u8;
            carry = sum >> 8;
        }
    }

    // Repeated division by 10 over base-256 digits, least significant first.
    let mut digits = Vec::new();
    while magnitude.iter().any(|byte| *byte != 0) {
        let mut remainder = 0u32;
        for byte in &mut magnitude {
            let current = (remainder << 8) | u32::from(*byte);
            *byte = (current / 10) as u8;
            remainder = current % 10;
        }
        digits.push(remainder as u8);
    }
    if digits.is_empty() {
        digits.push(0);
    }

    let mut out = String::with_capacity(digits.len() + 1);
    if negative {
        out.push('-');
    }
    for digit in digits.iter().rev() {
        out.push((b'0' + digit) as char);
    }
    out
}

fn format_decimal(decimal: &CqlDecimal) -> String {
    let (bytes, scale) = decimal.as_signed_be_bytes_slice_and_exponent();
    let digits = signed_be_to_decimal(bytes);
    let (sign, body) = match digits.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", digits.as_str()),
    };
    let scale = i64::from(scale);
    // Guard against an absurd scale allocating gigabytes.
    if !(-1000..=1000).contains(&scale) {
        return format!("{sign}{body}e{}", -scale);
    }
    if scale == 0 {
        return format!("{sign}{body}");
    }
    if scale < 0 {
        return format!("{sign}{body}{}", "0".repeat(scale.unsigned_abs() as usize));
    }
    let scale = scale as usize;
    if body.len() > scale {
        let (int, fraction) = body.split_at(body.len() - scale);
        format!("{sign}{int}.{fraction}")
    } else {
        format!("{sign}0.{}{body}", "0".repeat(scale - body.len()))
    }
}

fn format_date(days: u32) -> String {
    let (year, month, day) = civil_from_days(i64::from(days) - (1 << 31));
    format!("{year:04}-{month:02}-{day:02}")
}

fn format_time(nanos: i64) -> String {
    let seconds = nanos.div_euclid(1_000_000_000);
    let nanos = nanos.rem_euclid(1_000_000_000);
    let (hour, minute, second) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if nanos == 0 {
        format!("{hour:02}:{minute:02}:{second:02}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}.{nanos:09}")
    }
}

fn format_timestamp(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let day_millis = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let seconds = day_millis / 1000;
    let millis = day_millis % 1000;
    let (hour, minute, second) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if millis == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
    }
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use scylla::value::{Counter, CqlTimeuuid};
    use std::str::FromStr;

    fn as_json(value: CqlValue) -> Value {
        cql_to_json(&Some(value))
    }

    #[test]
    fn null_stays_null() {
        assert_eq!(cql_to_json(&None), Value::Null);
    }

    #[test]
    fn scalar_types() {
        assert_eq!(as_json(CqlValue::Boolean(true)), json!(true));
        assert_eq!(as_json(CqlValue::TinyInt(-3)), json!(-3));
        assert_eq!(as_json(CqlValue::SmallInt(-300)), json!(-300));
        assert_eq!(as_json(CqlValue::Int(-70_000)), json!(-70_000));
        assert_eq!(
            as_json(CqlValue::BigInt(-5_000_000_000)),
            json!(-5_000_000_000i64)
        );
        assert_eq!(as_json(CqlValue::Counter(Counter(42))), json!(42));
        assert_eq!(as_json(CqlValue::Float(1.5)), json!(1.5));
        assert_eq!(as_json(CqlValue::Double(-2.25)), json!(-2.25));
        assert_eq!(as_json(CqlValue::Text("héllo".to_owned())), json!("héllo"));
        assert_eq!(as_json(CqlValue::Ascii("plain".to_owned())), json!("plain"));
        assert_eq!(
            as_json(CqlValue::Inet("127.0.0.1".parse().unwrap())),
            json!("127.0.0.1")
        );
    }

    #[test]
    fn blob_becomes_byte_array() {
        assert_eq!(
            as_json(CqlValue::Blob(vec![0, 127, 255])),
            json!([0, 127, 255])
        );
    }

    #[test]
    fn uuid_and_timeuuid_become_strings() {
        let text = "3d4f8a2e-6b1c-11ee-8c99-0242ac120002";
        let timeuuid = CqlTimeuuid::from_str(text).unwrap();
        assert_eq!(
            as_json(CqlValue::Timeuuid(timeuuid)),
            json!("3d4f8a2e-6b1c-11ee-8c99-0242ac120002")
        );
        assert_eq!(
            as_json(CqlValue::Uuid(*timeuuid.as_ref())),
            json!("3d4f8a2e-6b1c-11ee-8c99-0242ac120002")
        );
    }

    #[test]
    fn temporal_types_are_iso_strings() {
        // Cassandra dates count days from 2^31 before the unix epoch.
        assert_eq!(
            as_json(CqlValue::Date(CqlDate(1 << 31))),
            json!("1970-01-01")
        );
        assert_eq!(
            as_json(CqlValue::Date(CqlDate((1 << 31) + 1))),
            json!("1970-01-02")
        );
        assert_eq!(
            as_json(CqlValue::Date(CqlDate((1 << 31) - 1))),
            json!("1969-12-31")
        );
        assert_eq!(
            as_json(CqlValue::Timestamp(CqlTimestamp(1_700_000_000_123))),
            json!("2023-11-14T22:13:20.123Z")
        );
        assert_eq!(
            as_json(CqlValue::Timestamp(CqlTimestamp(0))),
            json!("1970-01-01T00:00:00Z")
        );
        assert_eq!(as_json(CqlValue::Time(CqlTime(0))), json!("00:00:00"));
        let nanos = (12 * 3600 + 34 * 60 + 56) * 1_000_000_000 + 789_012_345;
        assert_eq!(
            as_json(CqlValue::Time(CqlTime(nanos))),
            json!("12:34:56.789012345")
        );
    }

    #[test]
    fn decimals_and_varints_are_decimal_strings() {
        // 1234 with scale 2 -> 12.34
        assert_eq!(
            as_json(CqlValue::Decimal(
                CqlDecimal::from_signed_be_bytes_and_exponent(vec![0x04, 0xD2], 2)
            )),
            json!("12.34")
        );
        // -1234 (two's complement 0xFB2E) with scale 2 -> -12.34
        assert_eq!(
            as_json(CqlValue::Decimal(
                CqlDecimal::from_signed_be_bytes_and_exponent(vec![0xFB, 0x2E], 2)
            )),
            json!("-12.34")
        );
        assert_eq!(
            as_json(CqlValue::Decimal(
                CqlDecimal::from_signed_be_bytes_and_exponent(vec![0x04, 0xD2], 4)
            )),
            json!("0.1234")
        );
        assert_eq!(
            as_json(CqlValue::Decimal(
                CqlDecimal::from_signed_be_bytes_and_exponent(vec![0x04, 0xD2], -2)
            )),
            json!("123400")
        );
        assert_eq!(
            as_json(CqlValue::Varint(
                scylla::value::CqlVarint::from_signed_bytes_be(vec![0x01, 0x00])
            )),
            json!("256")
        );
        assert_eq!(
            as_json(CqlValue::Varint(
                scylla::value::CqlVarint::from_signed_bytes_be(vec![0xFF, 0x00])
            )),
            json!("-256")
        );
    }

    #[test]
    fn collections_and_composites() {
        assert_eq!(
            as_json(CqlValue::List(vec![
                CqlValue::Int(1),
                CqlValue::Text("x".to_owned())
            ])),
            json!([1, "x"])
        );
        assert_eq!(
            as_json(CqlValue::Set(vec![CqlValue::Boolean(false)])),
            json!([false])
        );
        assert_eq!(
            as_json(CqlValue::Map(vec![(
                CqlValue::Text("a".to_owned()),
                CqlValue::Int(1)
            )])),
            json!({"a": 1})
        );
        // Non-string keys use their JSON rendering as the object key.
        assert_eq!(
            as_json(CqlValue::Map(vec![(
                CqlValue::Int(7),
                CqlValue::Text("v".to_owned())
            )])),
            json!({"7": "v"})
        );
        assert_eq!(
            as_json(CqlValue::Tuple(vec![Some(CqlValue::Int(1)), None])),
            json!([1, null])
        );
        assert_eq!(
            as_json(CqlValue::UserDefinedType {
                keyspace: "ks".to_owned(),
                name: "t".to_owned(),
                fields: vec![
                    ("a".to_owned(), Some(CqlValue::Int(1))),
                    ("b".to_owned(), None),
                ],
            }),
            json!({"a": 1, "b": null})
        );
        assert_eq!(
            as_json(CqlValue::Duration(scylla::value::CqlDuration {
                months: 1,
                days: 2,
                nanoseconds: 3,
            })),
            json!("1mo2d3ns")
        );
    }

    #[test]
    fn unknown_variant_falls_back_to_debug_string() {
        assert_eq!(as_json(CqlValue::Empty), json!("Empty"));
    }

    #[test]
    fn json_parameters_map_to_cql() {
        assert_eq!(json_to_cql(&Value::Null).unwrap(), None);
        assert_eq!(
            json_to_cql(&json!(true)).unwrap(),
            Some(CqlValue::Boolean(true))
        );
        assert_eq!(json_to_cql(&json!(42)).unwrap(), Some(CqlValue::BigInt(42)));
        assert_eq!(
            json_to_cql(&json!(-1.5)).unwrap(),
            Some(CqlValue::Double(-1.5))
        );
        assert_eq!(
            json_to_cql(&json!("text")).unwrap(),
            Some(CqlValue::Text("text".to_owned()))
        );
        assert_eq!(
            json_to_cql(&json!([0, 255])).unwrap(),
            Some(CqlValue::Blob(vec![0, 255]))
        );
        assert!(json_params(&[json!(1), Value::Null]).unwrap().len() == 2);
    }

    #[test]
    fn unsupported_parameters_are_rejected() {
        assert!(json_to_cql(&json!({"a": 1})).is_err());
        assert!(json_to_cql(&json!([256])).is_err());
        assert!(json_to_cql(&json!([-1])).is_err());
        assert!(json_to_cql(&json!(["x"])).is_err());
        // u64 above i64::MAX cannot be represented as a CQL bigint.
        assert!(json_to_cql(&json!(u64::MAX)).is_err());
    }
}

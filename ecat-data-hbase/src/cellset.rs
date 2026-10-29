// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! HBase REST JSON CellSet 的编解码。
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ecat_errors::Error;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::backend;

/// 一行扫描结果：(row key, cells)。
pub(crate) type Row = (String, BTreeMap<String, Value>);

/// `prefix` 的下一个字典序（exclusive endRow）；`None` = 无上界
/// （前缀全是 `0xFF`，或空前缀 = 从头扫）。
pub(crate) fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.last_mut() {
        if *last < u8::MAX {
            *last += 1;
            return Some(end);
        }
        end.pop();
    }
    None
}

/// 写入约定：字符串存原始 UTF-8 字节，其它值存 `serde_json` 序列化。
pub(crate) fn encode_value(value: &Value) -> Vec<u8> {
    match value {
        Value::String(s) => s.as_bytes().to_vec(),
        other => other.to_string().into_bytes(),
    }
}

/// 读取约定：UTF-8 且能解析成 JSON → JSON 值；其它 UTF-8 → 字符串；
/// 非 UTF-8 → Base-64 字符串。
fn decode_value(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string())),
        Err(_) => Value::String(B64.encode(bytes)),
    }
}

fn decode_key(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| B64.encode(e.into_bytes()))
}

fn b64_error(e: base64::DecodeError) -> Error {
    backend(format!("hbase: invalid base64 in CellSet: {e}"))
}

pub(crate) fn parse_cellset(body: &[u8]) -> Result<Vec<Row>, Error> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| backend(format!("hbase: invalid CellSet: {e}")))?;
    let Some(rows) = value.get("Row") else {
        return Ok(Vec::new());
    };
    let rows = rows
        .as_array()
        .ok_or_else(|| backend("hbase: CellSet.Row is not an array".into()))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let key = row
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| backend("hbase: CellSet row has no key".into()))?;
        let key = decode_key(B64.decode(key).map_err(b64_error)?);
        let mut cells = BTreeMap::new();
        if let Some(list) = row.get("Cell") {
            let list = list
                .as_array()
                .ok_or_else(|| backend("hbase: CellSet Cell is not an array".into()))?;
            for cell in list {
                let column = cell
                    .get("column")
                    .and_then(Value::as_str)
                    .ok_or_else(|| backend("hbase: CellSet cell has no column".into()))?;
                let data = cell
                    .get("$")
                    .and_then(Value::as_str)
                    .ok_or_else(|| backend("hbase: CellSet cell has no value".into()))?;
                let column = String::from_utf8(B64.decode(column).map_err(b64_error)?)
                    .map_err(|_| backend("hbase: non-UTF-8 cell column".into()))?;
                let data = B64.decode(data).map_err(b64_error)?;
                // 同列多版本时保留先到的（HBase 的 Result 按版本从新到旧排列）。
                cells.entry(column).or_insert_with(|| decode_value(&data));
            }
        }
        out.push((key, cells));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_end_handles_carry_and_ff_boundary() {
        assert_eq!(prefix_end(b""), None, "空前缀 = 从头扫描");
        assert_eq!(prefix_end(b"pre"), Some(b"prf".to_vec()));
        assert_eq!(prefix_end(&[0x61, 0xff]), Some(vec![0x62]));
        assert_eq!(prefix_end(&[0xff]), None, "全 0xFF 没有下一个字典序");
        assert_eq!(prefix_end(&[0x61, 0xff, 0xff]), Some(vec![0x62]));
    }

    /// 值约定：字符串 / JSON / 非 UTF-8 三条路径。
    #[test]
    fn value_convention_round_trips() {
        assert_eq!(
            decode_value(&encode_value(&Value::String("v1".into()))),
            "v1"
        );
        assert_eq!(decode_value(&encode_value(&serde_json::json!(42))), 42);
        assert_eq!(
            decode_value(&encode_value(&serde_json::json!({"x": 1}))),
            serde_json::json!({"x": 1})
        );
        assert_eq!(decode_value(&[0xff, 0xfe]), "//4=");
    }

    #[test]
    fn parses_cellset_and_rejects_malformed() {
        let body = br#"{"Row":[{"key":"cm93MQ==","Cell":[
            {"column":"Y2Y6YQ==","$":"djE="}]}]}"#;
        let rows = parse_cellset(body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "row1");
        assert_eq!(rows[0].1["cf:a"], "v1");

        assert!(parse_cellset(b"not json").is_err());
        assert!(parse_cellset(br#"{"Row":[{"key":"!!!","Cell":[]}]}"#).is_err());
    }
}

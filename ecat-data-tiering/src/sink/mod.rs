// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 冷层 sink 实现；每个后端一个 feature。
use serde_json::Value;

use crate::TieringError;

#[cfg(feature = "clickhouse")]
mod clickhouse;

#[cfg(feature = "clickhouse")]
pub use clickhouse::{ClickhouseSink, ClickhouseTable};

#[cfg(feature = "s3")]
mod s3;

#[cfg(feature = "s3")]
pub use s3::{S3Sink, S3Table};

/// 单个 JSON 值 → SQL 字面量（ClickHouse 语法，MySQL 式转义）。
///
/// 字符串/数组/对象按 MySQL 规则转义 `\` 与 `'`；数组与对象先序列化为 JSON 文本
/// 再作为字符串字面量。本函数只负责值，标识符必须另行走 `validate_ident`。
pub fn render_sql_value(v: &Value) -> Result<String, TieringError> {
    Ok(match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => sql_string(s),
        Value::Array(_) | Value::Object(_) => {
            let json = serde_json::to_string(v)
                .map_err(|e| TieringError::Cold(format!("failed to serialize SQL value: {e}")))?;
            sql_string(&json)
        }
    })
}

/// 单引号字符串字面量；逐字符转义，避免先转 `'` 再转 `\` 造成二次转义。
pub(crate) fn sql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

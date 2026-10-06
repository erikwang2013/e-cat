// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! `serde_json::Value` → tiberius 绑定值。

use serde_json::Value;
use std::borrow::Cow;
use tiberius::{ColumnData, ToSql};

/// 一条语句的全部绑定值。
///
/// tiberius 的 `query` / `execute` 收的是 `&[&dyn ToSql]`（**借用**），而
/// `serde_json::Value` 的借用活不到 `query` 返回的 `QueryStream` 那么久，所以先把
/// `Value` **物化**成自持所有权的本枚举，再由它构造引用切片（见 `client.rs` 的
/// `to_sql_refs`）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Bind {
    I64(i64),
    F64(f64),
    Str(String),
    Bool(bool),
    Null,
}

impl Bind {
    /// 分派规则与 `ecat-data-sqlx` 逐条对齐（那边是 `q.bind(...)` 的四分支）：
    /// 整数优先 `i64`，其次 `f64`，再不然落成文本。
    ///
    /// 整数优先是**顺序问题**而非偏好：`serde_json` 的 `Number` 同时装得下
    /// `i64` 与 `u64`，先问 `as_i64` 才不会把 `7` 变成 `7.0`（sqlx 路径的顺序）。
    pub(crate) fn from_json(v: &Value) -> Self {
        match v {
            Value::String(s) => Self::Str(s.clone()),
            Value::Number(n) => match n.as_i64() {
                Some(i) => Self::I64(i),
                // `u64` 超出 `i64::MAX` 时只能走 `f64`（有精度损失）—— 与 sqlx
                // 路径同一取舍；`as_f64` 也为 `None` 才落到文本。
                None => match n.as_f64() {
                    Some(f) => Self::F64(f),
                    None => Self::Str(n.to_string()),
                },
            },
            Value::Bool(b) => Self::Bool(*b),
            Value::Null => Self::Null,
            // 数组/对象没有 SQL 标量对应：落成 JSON 文本（sqlx 路径的 `_` 分支）。
            Value::Array(_) | Value::Object(_) => Self::Str(v.to_string()),
        }
    }
}

impl ToSql for Bind {
    /// 实现 `ToSql` 而不是给每个变体各取一次引用：`params` 要的是同质的
    /// `&[&dyn ToSql]`，五→TDS 的映射只此一处。
    fn to_sql(&self) -> ColumnData<'_> {
        match self {
            Self::I64(n) => ColumnData::I64(Some(*n)),
            Self::F64(n) => ColumnData::F64(Some(*n)),
            // `Cow::Borrowed`：值就在 `self` 里，活得比本次调用久。
            Self::Str(s) => ColumnData::String(Some(Cow::Borrowed(s))),
            Self::Bool(b) => ColumnData::Bit(Some(*b)),
            // JSON null 不带类型信息，而 TDS 参数**是强类型的** —— 必须挑一个。
            //
            // 选 nvarchar：`Option<String>` 实现 `ToSql` 后就是这个形态
            // （`tiberius-ng-0.13.1/src/macros.rs:53-63` 的 `to_sql!` 宏为
            // `Option<String>` 生成 `ColumnData::String(None)`），与 sqlx 路径的
            // `q.bind(None::<String>)` 是同一个选择。服务端见到的是带类型的 NULL，
            // 隐式转换到目标列，语义仍是 NULL。
            Self::Null => ColumnData::String(None),
        }
    }
}

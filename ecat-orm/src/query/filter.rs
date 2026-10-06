// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use serde_json::Value;

/// 比较操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
    In,
    NotIn,
    IsNull,
    NotNull,
}

impl Op {
    /// SQL 运算符。**不用于 `In` / `NotIn` / `IsNull` / `NotNull`** ——
    /// 那四个的 SQL 形态不是中缀运算符（见 `takes_value`）。
    pub fn as_sql(self) -> &'static str {
        match self {
            // ANSI 用 `<>`。`!=` 在 SQL Server 上可用但在某些 PG 兼容模式下不是
            // 标准写法 —— 统一走 `<>`，各后端都认。
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Like => "LIKE",
            Self::In | Self::NotIn | Self::IsNull | Self::NotNull => {
                unreachable!("`{}` has no infix SQL form", self.as_str())
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "Eq",
            Self::Ne => "Ne",
            Self::Lt => "Lt",
            Self::Le => "Le",
            Self::Gt => "Gt",
            Self::Ge => "Ge",
            Self::Like => "Like",
            Self::In => "In",
            Self::NotIn => "NotIn",
            Self::IsNull => "IsNull",
            Self::NotNull => "NotNull",
        }
    }

    /// 是否需要一个绑定值。`IsNull` / `NotNull` 不需要。
    pub fn takes_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::NotNull)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

impl Order {
    pub fn as_sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// JOIN 类型。只做 `INNER` / `LEFT` —— 见 spec §10 非目标，
/// 右连接与外连接不在本批范围。
///
/// **为什么是个枚举而不是一个 `&str`**：裸 `JOIN` 等价于 `INNER JOIN`，
/// 所以「左连接少写了 `LEFT`」不报错、只给错结果。类型化了才有人替我们把关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    Left,
}

impl JoinType {
    /// 完整的连接关键字（含 `JOIN`），供渲染时直接用。
    pub fn as_sql(self) -> &'static str {
        match self {
            Self::Inner => "INNER JOIN",
            Self::Left => "LEFT JOIN",
        }
    }
}

/// WHERE 子句的一项。
///
/// **`Raw` 是逃生口**：关联表的列不在本实体的 `EntityMeta.columns` 里，
/// 白名单校验拦不住也不该拦（我们根本没有关联实体的元数据）。用它时
/// **输入必须可信** —— 它不做任何校验，直接拼进 SQL。
#[derive(Debug, Clone)]
pub enum Expr {
    Cmp {
        column: String,
        op: Op,
        value: Value,
    },
    In {
        column: String,
        values: Vec<Value>,
        negated: bool,
    },
    Null {
        column: String,
        negated: bool,
    },
    Raw(String),
}

/// ORDER BY 的一项。
#[derive(Debug, Clone)]
pub struct OrderBy {
    pub column: String,
    pub dir: Order,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_maps_to_sql_operators() {
        assert_eq!(Op::Eq.as_sql(), "=");
        assert_eq!(Op::Ne.as_sql(), "<>");
        assert_eq!(Op::Lt.as_sql(), "<");
        assert_eq!(Op::Le.as_sql(), "<=");
        assert_eq!(Op::Gt.as_sql(), ">");
        assert_eq!(Op::Ge.as_sql(), ">=");
        assert_eq!(Op::Like.as_sql(), "LIKE");
    }

    /// `Ne` 必须是 `<>` 而不是 `!=` —— `!=` 不是标准 SQL，
    /// SQL Server 与部分 PG 配置下会直接语法错误。
    #[test]
    fn ne_is_ansi_not_bang_eq() {
        assert_ne!(Op::Ne.as_sql(), "!=");
    }

    #[test]
    fn op_reports_whether_it_takes_a_value() {
        assert!(Op::Eq.takes_value());
        assert!(!Op::IsNull.takes_value());
        assert!(!Op::NotNull.takes_value());
        assert!(Op::In.takes_value());
    }
}

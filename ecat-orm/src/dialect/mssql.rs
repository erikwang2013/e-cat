// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! SQL Server (T-SQL)。与其余三个方言差异最大：分页片段是 **SELECT 前缀**
//! （`TOP`），且没有 `CREATE TABLE IF NOT EXISTS`。

use super::DialectSpec;
use super::InsertPlan;
use super::Limit;
use crate::entity::ColType;

pub(crate) struct MssqlSpec;

impl DialectSpec for MssqlSpec {
    fn quote(&self, ident: &str) -> String {
        format!("[{ident}]")
    }

    fn placeholder(&self, index: usize) -> String {
        format!("@P{index}")
    }

    fn limit_clause(&self, limit: u64, offset: u64, has_order: bool) -> Limit {
        if offset == 0 {
            // `TOP` 插在 SELECT 与列清单之间 —— 这是 `Limit { prefix, suffix }`
            // 存在（裁决 C）的全部理由。
            return Limit {
                prefix: format!("TOP ({limit}) "),
                suffix: String::new(),
            };
        }
        // OFFSET/FETCH 强制要求 ORDER BY —— 用户没给就补一个常量序，
        // 否则 SQL Server 直接语法报错。
        let order = if has_order {
            ""
        } else {
            " ORDER BY (SELECT NULL)"
        };
        Limit {
            prefix: String::new(),
            suffix: format!("{order} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY"),
        }
    }

    /// 一步式：`INSERT ... OUTPUT INSERTED.pk`。**没有** MySQL 的
    /// `LAST_INSERT_ID()` 连接作用域问题。
    fn insert_plan(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> InsertPlan {
        InsertPlan::Single {
            sql: format!(
                "INSERT INTO {} ({}) OUTPUT INSERTED.{} VALUES ({})",
                self.quote(table),
                cols.iter()
                    .map(|c| self.quote(c))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.quote(pk),
                (1..=cols.len())
                    .map(|i| self.placeholder(i))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn upsert(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> String {
        let cols_sql = cols
            .iter()
            .map(|c| self.quote(c))
            .collect::<Vec<_>>()
            .join(", ");
        let src_cols = cols
            .iter()
            .map(|c| format!("[s].{}", self.quote(c)))
            .collect::<Vec<_>>()
            .join(", ");
        let assigns = cols
            .iter()
            .filter(|c| c.as_str() != pk)
            .map(|c| {
                let q = self.quote(c);
                format!("[t].{q} = [s].{q}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let ph = (1..=cols.len())
            .map(|i| self.placeholder(i))
            .collect::<Vec<_>>()
            .join(", ");
        let pk_q = self.quote(pk);
        format!(
            "MERGE INTO {} AS [t] USING (VALUES ({ph})) AS [s] ({cols_sql}) \
             ON [t].{pk_q} = [s].{pk_q} WHEN MATCHED THEN UPDATE SET {assigns} \
             WHEN NOT MATCHED THEN INSERT ({cols_sql}) VALUES ({src_cols});",
            self.quote(table)
        )
    }

    fn bool_literal(&self, b: bool) -> String {
        if b { "1".into() } else { "0".into() }
    }

    fn col_type(&self, ty: ColType) -> String {
        match ty {
            ColType::I64 => "BIGINT".into(),
            ColType::I32 => "INT".into(),
            ColType::F64 => "FLOAT".into(),
            ColType::Bool => "BIT".into(),
            // **不是 TEXT**：SQL Server 的 `TEXT` 已弃用，且只有 NVARCHAR 是 Unicode。
            ColType::Text | ColType::Json => "NVARCHAR(MAX)".into(),
            ColType::Bytes => "VARBINARY(MAX)".into(),
            ColType::Timestamp => "DATETIME2".into(),
            ColType::Date => "DATE".into(),
        }
    }

    /// SQL Server **没有** `CREATE TABLE IF NOT EXISTS` —— 返回空串，
    /// 由 `migrate::ddl` 改为先查 `INFORMATION_SCHEMA.TABLES`。
    fn table_exists_sql(&self, _table: &str) -> String {
        String::new()
    }

    fn autoincrement_ddl(&self, ty: ColType) -> String {
        format!("{} IDENTITY(1,1)", self.col_type(ty))
    }

    fn max_params_per_stmt(&self) -> usize {
        // SQL Server 单语句最多 2100 个参数 —— 四个后端里最紧的。
        2100
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColType;

    fn s() -> MssqlSpec {
        MssqlSpec
    }

    /// 方括号，不是双引号、不是反引号。
    #[test]
    fn quotes_with_brackets() {
        assert_eq!(s().quote("user"), "[user]");
    }

    #[test]
    fn placeholder_is_at_p_n() {
        assert_eq!(s().placeholder(1), "@P1");
        assert_eq!(s().placeholder(12), "@P12");
    }

    /// **无 OFFSET 时走 TOP（前缀）**，这是「裁决 C」存在的全部理由。
    #[test]
    fn limit_without_offset_uses_top_prefix() {
        let l = s().limit_clause(10, 0, false);
        assert_eq!(l.prefix, "TOP (10) ");
        assert_eq!(l.suffix, "");
    }

    /// 有 OFFSET 时走 OFFSET/FETCH（后缀）；无 ORDER BY 必须补一个，
    /// 否则 SQL Server 直接语法报错（ORDER BY 是 OFFSET/FETCH 的强制前提）。
    #[test]
    fn limit_with_offset_uses_offset_fetch_and_synthesizes_order_by() {
        let l = s().limit_clause(10, 20, false);
        assert_eq!(l.prefix, "");
        assert!(
            l.suffix.contains("ORDER BY (SELECT NULL)"),
            "got: {}",
            l.suffix
        );
        assert!(l.suffix.contains("OFFSET 20 ROWS"), "got: {}", l.suffix);
        assert!(
            l.suffix.contains("FETCH NEXT 10 ROWS ONLY"),
            "got: {}",
            l.suffix
        );
    }

    #[test]
    fn existing_order_by_is_not_duplicated() {
        let l = s().limit_clause(10, 20, true);
        assert!(
            !l.suffix.contains("ORDER BY (SELECT NULL)"),
            "用户已有 ORDER BY 时不得再补: {}",
            l.suffix
        );
    }

    /// INSERT ... OUTPUT INSERTED.pk 是一步式，无 LAST_INSERT_ID 的连接作用域问题。
    #[test]
    fn insert_uses_output_inserted() {
        match s().insert_plan("users", &["name".into()], "id", 1) {
            InsertPlan::Single { sql } => {
                assert!(sql.contains("OUTPUT INSERTED.[id]"), "got: {sql}");
            }
            other => panic!("MSSQL 应走一步式 OUTPUT，得到 {other:?}"),
        }
    }

    #[test]
    fn upsert_uses_merge() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("MERGE"), "got: {sql}");
    }

    #[test]
    fn bool_literal_is_1_and_0() {
        assert_eq!(s().bool_literal(true), "1");
        assert_eq!(s().bool_literal(false), "0");
    }

    #[test]
    fn column_types_match_the_matrix() {
        assert_eq!(s().col_type(ColType::Timestamp), "DATETIME2");
        assert_eq!(s().col_type(ColType::Text), "NVARCHAR(MAX)");
        assert_eq!(s().col_type(ColType::I64), "BIGINT");
        assert_eq!(s().col_type(ColType::Bytes), "VARBINARY(MAX)");
    }

    /// SQL Server 没有 CREATE TABLE IF NOT EXISTS —— 返回空串，
    /// 由 migrate::ddl 改为先查 INFORMATION_SCHEMA.TABLES。
    ///
    /// 断言精确到空串而非仅「不含 IF NOT EXISTS」：后者对空串是**恒真**的，
    /// 任何返回「没有 IF NOT EXISTS 的任意串」的实现都能骗过它。
    #[test]
    fn create_table_has_no_if_not_exists() {
        assert_eq!(s().table_exists_sql("users"), "");
        assert!(!s().table_exists_sql("users").contains("IF NOT EXISTS"));
    }

    #[test]
    fn autoincrement_uses_identity() {
        assert_eq!(s().autoincrement_ddl(ColType::I64), "BIGINT IDENTITY(1,1)");
    }

    /// 2100 是四个后端里最紧的 —— 写错成 65535 会让批量插入在真库上炸。
    #[test]
    fn max_params_is_2100() {
        assert_eq!(s().max_params_per_stmt(), 2100);
    }
}

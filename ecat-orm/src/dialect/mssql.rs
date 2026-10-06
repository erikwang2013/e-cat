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

    fn limit_clause(&self, limit: Option<u64>, offset: u64, has_order: bool) -> Limit {
        if offset == 0 {
            return match limit {
                // `TOP` 插在 SELECT 与列清单之间 —— 这是 `Limit { prefix, suffix }`
                // 存在（裁决 C）的全部理由。
                Some(l) => Limit {
                    prefix: format!("TOP ({l}) "),
                    suffix: String::new(),
                },
                // 无 limit 无 offset：`build_select` 不会走到这里（它在两者都没设时
                // 直接取空片段），真走到也**不得**产出哨兵值 —— 给空片段。
                None => Limit::none(),
            };
        }
        // OFFSET/FETCH 强制要求 ORDER BY —— 用户没给就补一个常量序，
        // 否则 SQL Server 直接语法报错。`None`（只设 offset）时**省略 FETCH**：
        // `FETCH NEXT 18446744073709551615 ROWS ONLY` 超出 BIGINT，真库拒收。
        let order = if has_order {
            ""
        } else {
            " ORDER BY (SELECT NULL)"
        };
        let fetch = match limit {
            Some(l) => format!(" FETCH NEXT {l} ROWS ONLY"),
            None => String::new(),
        };
        Limit {
            prefix: String::new(),
            suffix: format!("{order} OFFSET {offset} ROWS{fetch}"),
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
        // 可更新列集为空（只有主键列的表）时 `WHEN MATCHED THEN UPDATE SET ` 后面
        // 没有赋值 —— 空 SET，真库语法错误。**整段去掉** MATCHED 分支：
        // 没有列可更新时语义是「已存在就不动」，即**纯插入**。
        // 不能改成 `WHEN MATCHED THEN DELETE` —— 那会把已有的行删掉。
        let matched = if assigns.is_empty() {
            String::new()
        } else {
            format!("WHEN MATCHED THEN UPDATE SET {assigns} ")
        };
        format!(
            "MERGE INTO {} AS [t] USING (VALUES ({ph})) AS [s] ({cols_sql}) \
             ON [t].{pk_q} = [s].{pk_q} {matched}\
             WHEN NOT MATCHED THEN INSERT ({cols_sql}) VALUES ({src_cols});",
            self.quote(table)
        )
    }

    /// T-SQL 的 `UPDATE … FROM (VALUES …) AS v(…)` 与 ANSI 同形，
    /// 只差方括号引号与 `@Pn` 占位符（都由 `self` 提供）。
    fn update_many_stmt(
        &self,
        table: &str,
        pk: &str,
        set_cols: &[String],
        where_extra: &[String],
        n_rows: usize,
    ) -> String {
        super::update_many_from_values(self, table, pk, set_cols, where_extra, n_rows)
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

    /// SQL Server **没有** `CREATE TABLE IF NOT EXISTS` —— 但前缀仍是完整的
    /// `CREATE TABLE [x]`，只是调用方得先自查存在性
    /// （见 [`DialectSpec::needs_exists_check_before_create`]）。
    ///
    /// **早期实现这里返回空串**，调用方 `format!("{} ({cols})", …)` 拼出来的
    /// SQL 没有 `CREATE TABLE` 关键字、是真库拒收的非法语句 —— 而「不含
    /// `IF NOT EXISTS`」断言对空串恒真，抓不到（Task 17 必做之一）。
    fn create_table_prefix(&self, table: &str) -> String {
        format!("CREATE TABLE {}", self.quote(table))
    }

    fn needs_exists_check_before_create(&self) -> bool {
        true
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
        let l = s().limit_clause(Some(10), 0, false);
        assert_eq!(l.prefix, "TOP (10) ");
        assert_eq!(l.suffix, "");
    }

    /// **只设 offset 不设 limit**：`None` 时必须**省略 FETCH** ——
    /// `FETCH NEXT 18446744073709551615 ROWS ONLY` 超出 BIGINT，真库拒收。
    /// ORDER BY 仍要补：OFFSET 强制要求它。
    #[test]
    fn offset_without_limit_omits_the_fetch() {
        let l = s().limit_clause(None, 20, false);
        assert_eq!(l.prefix, "");
        assert!(
            l.suffix.contains("ORDER BY (SELECT NULL)"),
            "got: {}",
            l.suffix
        );
        assert!(l.suffix.contains("OFFSET 20 ROWS"), "got: {}", l.suffix);
        assert!(!l.suffix.contains("FETCH"), "got: {}", l.suffix);
        assert!(
            !l.suffix.contains("18446744073709551615") && !l.suffix.contains("9223372036854775807"),
            "分页片段里不得出现哨兵数字: {}",
            l.suffix
        );
    }

    /// 有 OFFSET 时走 OFFSET/FETCH（后缀）；无 ORDER BY 必须补一个，
    /// 否则 SQL Server 直接语法报错（ORDER BY 是 OFFSET/FETCH 的强制前提）。
    #[test]
    fn limit_with_offset_uses_offset_fetch_and_synthesizes_order_by() {
        let l = s().limit_clause(Some(10), 20, false);
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
        let l = s().limit_clause(Some(10), 20, true);
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

    /// 多行 UPDATE：T-SQL 的 `UPDATE … FROM (VALUES …)` 与 ANSI 同形，
    /// 只差方括号引号与 `@Pn` 占位符。
    #[test]
    fn update_many_uses_from_values() {
        let sql = s().update_many_stmt("docs", "id", &["name".into()], &[], 1);
        assert_eq!(
            sql,
            "UPDATE [docs] SET [name] = v.[name] FROM (VALUES (@P1, @P2)) AS v([id], [name]) \
             WHERE [docs].[id] = v.[id]"
        );
    }

    /// **只有主键列时**：可更新列集为空 → 原实现产出
    /// `WHEN MATCHED THEN UPDATE SET  WHEN NOT MATCHED ...` —— 空 SET 之后紧跟
    /// 下一个 WHEN，整条语句错位、真库语法错误。
    /// 修法是**整段去掉 MATCHED 分支**：没有列可更新时语义是「已存在就不动」，
    /// 即纯插入（不是 `WHEN MATCHED THEN DELETE` —— 那会删掉已有的行）。
    #[test]
    fn upsert_with_only_the_pk_column_drops_the_matched_branch() {
        let sql = s().upsert("users", &["id".into()], "id", 1);
        assert_eq!(
            sql,
            "MERGE INTO [users] AS [t] USING (VALUES (@P1)) AS [s] ([id]) \
             ON [t].[id] = [s].[id] \
             WHEN NOT MATCHED THEN INSERT ([id]) VALUES ([s].[id]);"
        );
        assert!(
            !sql.contains("WHEN MATCHED"),
            "不得留下空的 SET 分支: {sql}"
        );
        assert!(!sql.contains("DELETE"), "空集时是纯插入，不是删除: {sql}");
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

    /// SQL Server 没有 `CREATE TABLE IF NOT EXISTS` —— 但前缀必须是**完整语句
    /// 的开头**，不能是空串（旧实现返回空串，拼出来的 SQL 没有 CREATE TABLE
    /// 关键字；而「不含 IF NOT EXISTS」对空串恒真、拦不住）。
    ///
    /// 断言精确到整串而非仅「不含 IF NOT EXISTS」：后者对空串是**恒真**的，
    /// 任何返回「没有 IF NOT EXISTS 的任意串」的实现都能骗过它。
    #[test]
    fn create_table_prefix_is_complete_without_if_not_exists() {
        let prefix = s().create_table_prefix("users");
        assert_eq!(prefix, "CREATE TABLE [users]", "MSSQL 也必须给出完整前缀");
        assert!(!prefix.contains("IF NOT EXISTS"), "got: {prefix}");
        assert!(
            s().needs_exists_check_before_create(),
            "MSSQL 必须自查存在性"
        );
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

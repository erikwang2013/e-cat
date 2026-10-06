// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! MySQL / MariaDB。

use super::DialectSpec;
use super::InsertPlan;
use super::Limit;
use crate::entity::ColType;

pub(crate) struct MySqlSpec;

impl DialectSpec for MySqlSpec {
    fn quote(&self, ident: &str) -> String {
        format!("`{ident}`")
    }

    fn placeholder(&self, _index: usize) -> String {
        "?".into()
    }

    fn limit_clause(&self, limit: Option<u64>, offset: u64, _has_order: bool) -> Limit {
        Limit {
            prefix: String::new(),
            suffix: match (limit, offset) {
                // `None` = 不限制行数：整个 LIMIT 子句省略（见 trait 的文档）。
                (None, 0) => String::new(),
                (None, o) => format!(" OFFSET {o}"),
                (Some(l), 0) => format!(" LIMIT {l}"),
                (Some(l), o) => format!(" LIMIT {l} OFFSET {o}"),
            },
        }
    }

    /// **两步式**。`LAST_INSERT_ID()` 是**连接作用域**的：在连接池下
    /// `INSERT` 与 `SELECT LAST_INSERT_ID()` 是两次独立的池取用，可能落到不同连接，
    /// 取回**别的会话**的 id（静默错值）。一步式在 MySQL 上没有等价写法。
    /// 调用方（`crud::insert`）必须把两条语句包进同一事务。
    fn insert_plan(&self, table: &str, cols: &[String], _pk: &str, _n_params: usize) -> InsertPlan {
        InsertPlan::InsertThen {
            insert: format!(
                "INSERT INTO {} ({}) VALUES ({})",
                self.quote(table),
                cols.iter()
                    .map(|c| self.quote(c))
                    .collect::<Vec<_>>()
                    .join(", "),
                (1..=cols.len())
                    .map(|i| self.placeholder(i))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            fetch: "SELECT LAST_INSERT_ID()".into(),
        }
    }

    fn upsert(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> String {
        let assigns = cols
            .iter()
            .filter(|c| c.as_str() != pk)
            .map(|c| {
                let q = self.quote(c);
                format!("{q} = VALUES({q})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        // 可更新列集为空（只有主键列的表）时 `ON DUPLICATE KEY UPDATE ` 后面
        // 什么都不剩 —— 空赋值列表，真库语法错误。MySQL 没有 `DO NOTHING`，
        // 用**主键自赋值**（`id = id`，恒等、无操作）表达「已存在就不动」。
        let assigns = if assigns.is_empty() {
            let q = self.quote(pk);
            format!("{q} = {q}")
        } else {
            assigns
        };
        format!(
            "INSERT INTO {} ({}) VALUES ({}) ON DUPLICATE KEY UPDATE {assigns}",
            self.quote(table),
            cols.iter()
                .map(|c| self.quote(c))
                .collect::<Vec<_>>()
                .join(", "),
            (1..=cols.len())
                .map(|i| self.placeholder(i))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// MySQL **没有** `UPDATE … FROM (VALUES …)` —— 用 JOIN 一个派生表表达：
    /// `SELECT ? AS col UNION ALL SELECT ?, ?`，第一行给出列名。
    fn update_many_stmt(
        &self,
        table: &str,
        pk: &str,
        set_cols: &[String],
        where_extra: &[String],
        n_rows: usize,
    ) -> String {
        let alias = super::update_many_aliases(self, pk, set_cols, where_extra);
        let per_row = alias.len();
        let selects = (0..n_rows)
            .map(|r| {
                let cols = (0..per_row)
                    .map(|c| {
                        let ph = self.placeholder(r * per_row + c + 1);
                        // 派生表的列名取自**第一个** SELECT 的别名，
                        // 后续分支再写一遍是死重量。
                        if r == 0 {
                            format!("{ph} AS {}", alias[c])
                        } else {
                            ph
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("SELECT {cols}")
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let q_table = self.quote(table);
        let sets = set_cols
            .iter()
            .map(|c| {
                let q = self.quote(c);
                format!("{q_table}.{q} = v.{q}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut sql = format!(
            "UPDATE {q_table} JOIN ({selects}) AS v ON {q_table}.{} = v.{} SET {sets}",
            self.quote(pk),
            self.quote(pk)
        );
        if !where_extra.is_empty() {
            let conds = where_extra
                .iter()
                .map(|c| {
                    format!(
                        "{q_table}.{} = v.{}",
                        self.quote(c),
                        self.quote(&format!("{c}__old"))
                    )
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            sql.push_str(&format!(" WHERE {conds}"));
        }
        sql
    }

    fn bool_literal(&self, b: bool) -> String {
        if b { "1".into() } else { "0".into() }
    }

    fn col_type(&self, ty: ColType) -> String {
        match ty {
            ColType::I64 => "BIGINT".into(),
            ColType::I32 => "INT".into(),
            ColType::F64 => "DOUBLE".into(),
            ColType::Bool => "TINYINT(1)".into(),
            ColType::Text | ColType::Json => "TEXT".into(),
            ColType::Bytes => "BLOB".into(),
            ColType::Timestamp => "DATETIME".into(),
            ColType::Date => "DATE".into(),
        }
    }

    fn create_table_prefix(&self, table: &str) -> String {
        format!("CREATE TABLE IF NOT EXISTS {}", self.quote(table))
    }

    fn autoincrement_ddl(&self, ty: ColType) -> String {
        format!("{} AUTO_INCREMENT", self.col_type(ty))
    }

    fn max_params_per_stmt(&self) -> usize {
        // max_allowed_packet 之外还有 65535 的语句参数上限。
        65535
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColType;

    fn s() -> MySqlSpec {
        MySqlSpec
    }

    /// MySQL 用反引号，不是双引号 —— 这是三个方言里唯一不同的引号。
    #[test]
    fn quotes_with_backticks() {
        assert_eq!(s().quote("user"), "`user`");
    }

    #[test]
    fn placeholder_is_question_mark() {
        assert_eq!(s().placeholder(1), "?");
        assert_eq!(s().placeholder(9), "?");
    }

    /// **本测试是 Task 10 里最重要的一条。**
    /// MySQL 的 LAST_INSERT_ID() 是连接作用域的，池下两次取用可能落到不同连接
    /// → 静默取回别的会话的值。所以必须走两步式，且两条语句由调用方包进同一事务。
    #[test]
    fn insert_is_two_step_not_returning() {
        match s().insert_plan("users", &["name".into()], "id", 1) {
            InsertPlan::InsertThen { insert, fetch } => {
                assert_eq!(insert, "INSERT INTO `users` (`name`) VALUES (?)");
                assert!(
                    fetch.contains("LAST_INSERT_ID()"),
                    "fetch 必须是 LAST_INSERT_ID()，得到: {fetch}"
                );
            }
            InsertPlan::Single { sql } => {
                panic!("MySQL 不得走一步式 —— 池下会静默取回别的连接的 id。得到: {sql}")
            }
        }
    }

    #[test]
    fn limit_is_a_suffix() {
        let l = s().limit_clause(Some(10), 0, false);
        assert_eq!(l.prefix, "");
        assert_eq!(l.suffix, " LIMIT 10");
        assert_eq!(
            s().limit_clause(Some(10), 20, false).suffix,
            " LIMIT 10 OFFSET 20"
        );
    }

    /// **只设 offset 不设 limit**：`None` 时整个 `LIMIT` 子句必须**省略**。
    /// 用 `u64::MAX` 顶替「未设置」会产出
    /// `LIMIT 18446744073709551615 OFFSET 20` —— 超出 BIGINT，真库拒收。
    #[test]
    fn offset_without_limit_omits_the_limit_clause() {
        let l = s().limit_clause(None, 20, false);
        assert_eq!(l.prefix, "");
        assert_eq!(l.suffix, " OFFSET 20");
        assert!(
            !l.suffix.contains("18446744073709551615") && !l.suffix.contains("9223372036854775807"),
            "分页片段里不得出现哨兵数字: {}",
            l.suffix
        );
    }

    /// 多行 UPDATE：MySQL **没有** `UPDATE … FROM (VALUES …)`，
    /// 用 JOIN 一个派生表表达（`SELECT ? AS col UNION ALL …`）。
    #[test]
    fn update_many_uses_a_joined_derived_table() {
        let sql = s().update_many_stmt("docs", "id", &["name".into()], &[], 2);
        assert_eq!(
            sql,
            "UPDATE `docs` JOIN (SELECT ? AS `id`, ? AS `name` UNION ALL SELECT ?, ?) AS v \
             ON `docs`.`id` = v.`id` SET `docs`.`name` = v.`name`"
        );
    }

    /// 带逐行 WHERE 比对列时，条件放在 `SET` 之后的 `WHERE`；
    /// 别名加 `__old` 后缀，与 SET 里的同名列区分开。
    #[test]
    fn update_many_puts_where_extras_after_the_set_clause() {
        let sql = s().update_many_stmt("docs", "id", &["name".into()], &["version".into()], 1);
        assert_eq!(
            sql,
            "UPDATE `docs` JOIN (SELECT ? AS `id`, ? AS `name`, ? AS `version__old`) AS v \
             ON `docs`.`id` = v.`id` SET `docs`.`name` = v.`name` \
             WHERE `docs`.`version` = v.`version__old`"
        );
    }

    #[test]
    fn upsert_uses_on_duplicate_key() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("ON DUPLICATE KEY UPDATE"), "got: {sql}");
        assert!(sql.contains("`name` = VALUES(`name`)"), "got: {sql}");
    }

    /// **只有主键列时**：可更新列集为空 → 原实现产出
    /// `ON DUPLICATE KEY UPDATE `（空赋值列表，真库语法错误）。
    /// MySQL 没有 `DO NOTHING`，用**主键自赋值**（无操作）表达同一语义。
    #[test]
    fn upsert_with_only_the_pk_column_is_a_self_assignment() {
        let sql = s().upsert("users", &["id".into()], "id", 1);
        assert_eq!(
            sql,
            "INSERT INTO `users` (`id`) VALUES (?) ON DUPLICATE KEY UPDATE `id` = `id`"
        );
        assert!(
            !sql.trim_end().ends_with("UPDATE"),
            "不得留下空的赋值列表: {sql}"
        );
    }

    #[test]
    fn bool_literal_is_1_and_0() {
        assert_eq!(s().bool_literal(true), "1");
        assert_eq!(s().bool_literal(false), "0");
    }

    #[test]
    fn column_types_match_the_matrix() {
        assert_eq!(s().col_type(ColType::Timestamp), "DATETIME");
        assert_eq!(s().col_type(ColType::I64), "BIGINT");
        assert_eq!(s().col_type(ColType::Text), "TEXT");
        assert_eq!(s().col_type(ColType::Bytes), "BLOB");
    }

    #[test]
    fn autoincrement_ddl_uses_auto_increment() {
        assert_eq!(s().autoincrement_ddl(ColType::I64), "BIGINT AUTO_INCREMENT");
    }

    #[test]
    fn create_table_prefix_uses_if_not_exists() {
        assert_eq!(
            s().create_table_prefix("users"),
            "CREATE TABLE IF NOT EXISTS `users`"
        );
    }
}

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

    fn limit_clause(&self, limit: u64, offset: u64, _has_order: bool) -> Limit {
        Limit {
            prefix: String::new(),
            suffix: if offset == 0 {
                format!(" LIMIT {limit}")
            } else {
                format!(" LIMIT {limit} OFFSET {offset}")
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

    fn table_exists_sql(&self, table: &str) -> String {
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
    fn upsert_uses_on_duplicate_key() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("ON DUPLICATE KEY UPDATE"), "got: {sql}");
        assert!(sql.contains("`name` = VALUES(`name`)"), "got: {sql}");
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
    fn table_exists_uses_if_not_exists() {
        assert_eq!(
            s().table_exists_sql("users"),
            "CREATE TABLE IF NOT EXISTS `users`"
        );
    }
}

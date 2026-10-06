// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use super::DialectSpec;
use super::InsertPlan;
use super::Limit;
use crate::entity::ColType;

pub(crate) struct SqliteSpec;

impl DialectSpec for SqliteSpec {
    fn quote(&self, ident: &str) -> String {
        format!("\"{ident}\"")
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

    fn insert_plan(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> InsertPlan {
        // SQLite 3.35+ 支持 RETURNING（随 sqlx 的 bundled 版本提供）。
        InsertPlan::Single {
            sql: format!(
                "INSERT INTO {} ({}) VALUES ({}) RETURNING {}",
                self.quote(table),
                cols.iter()
                    .map(|c| self.quote(c))
                    .collect::<Vec<_>>()
                    .join(", "),
                (1..=cols.len())
                    .map(|i| self.placeholder(i))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.quote(pk)
            ),
        }
    }

    fn upsert(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> String {
        let assigns = cols
            .iter()
            .filter(|c| c.as_str() != pk)
            .map(|c| {
                let q = self.quote(c);
                format!("{q} = excluded.{q}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO UPDATE SET {assigns}",
            self.quote(table),
            cols.iter()
                .map(|c| self.quote(c))
                .collect::<Vec<_>>()
                .join(", "),
            (1..=cols.len())
                .map(|i| self.placeholder(i))
                .collect::<Vec<_>>()
                .join(", "),
            self.quote(pk)
        )
    }

    fn bool_literal(&self, b: bool) -> String {
        if b { "1".into() } else { "0".into() }
    }

    fn col_type(&self, ty: ColType) -> String {
        // SQLite 是动态类型：列类型只是「亲和性」提示。时间存 RFC3339 文本、
        // 布尔存 1/0，都取 INTEGER/TEXT。
        match ty {
            ColType::I64 | ColType::I32 | ColType::Bool => "INTEGER".into(),
            ColType::F64 => "REAL".into(),
            ColType::Text | ColType::Json | ColType::Timestamp | ColType::Date => "TEXT".into(),
            ColType::Bytes => "BLOB".into(),
        }
    }

    fn table_exists_sql(&self, table: &str) -> String {
        format!("CREATE TABLE IF NOT EXISTS {}", self.quote(table))
    }

    fn autoincrement_ddl(&self, _ty: ColType) -> String {
        // SQLite 只允许 INTEGER PRIMARY KEY AUTOINCREMENT 这一种拼法 ——
        // 换成 BIGINT 就**不再是 rowid 别名**，自增静默失效。
        "INTEGER PRIMARY KEY AUTOINCREMENT".into()
    }

    fn max_params_per_stmt(&self) -> usize {
        // SQLITE_MAX_VARIABLE_NUMBER：3.32 之前是 999，之后是 32766。
        // 取保守值 999 —— 多切几块只是多几次往返，猜大了是运行期报错。
        999
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColType;

    fn s() -> SqliteSpec {
        SqliteSpec
    }

    #[test]
    fn quotes_with_double_quotes() {
        assert_eq!(s().quote("user"), "\"user\"");
    }

    #[test]
    fn placeholder_ignores_the_index() {
        assert_eq!(s().placeholder(1), "?");
        assert_eq!(s().placeholder(42), "?");
    }

    #[test]
    fn limit_is_a_suffix() {
        let l = s().limit_clause(10, 0, false);
        assert_eq!(l.prefix, "");
        assert_eq!(l.suffix, " LIMIT 10");
    }

    #[test]
    fn limit_with_offset() {
        let l = s().limit_clause(10, 20, false);
        assert_eq!(l.suffix, " LIMIT 10 OFFSET 20");
    }

    #[test]
    fn insert_uses_returning() {
        let plan = s().insert_plan("users", &["name".into()], "id", 1);
        match plan {
            InsertPlan::Single { sql } => {
                assert_eq!(
                    sql,
                    "INSERT INTO \"users\" (\"name\") VALUES (?) RETURNING \"id\""
                );
            }
            other => panic!("sqlite 应走一步式 RETURNING，得到 {other:?}"),
        }
    }

    #[test]
    fn upsert_uses_on_conflict() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("ON CONFLICT"), "got: {sql}");
        assert!(sql.contains("DO UPDATE"), "got: {sql}");
    }

    #[test]
    fn bool_literal_is_1_and_0() {
        assert_eq!(s().bool_literal(true), "1");
        assert_eq!(s().bool_literal(false), "0");
    }

    #[test]
    fn column_types_match_the_matrix() {
        assert_eq!(s().col_type(ColType::I64), "INTEGER");
        assert_eq!(s().col_type(ColType::Text), "TEXT");
        assert_eq!(s().col_type(ColType::Timestamp), "TEXT");
        assert_eq!(s().col_type(ColType::Date), "TEXT");
        assert_eq!(s().col_type(ColType::Bool), "INTEGER");
        assert_eq!(s().col_type(ColType::F64), "REAL");
        assert_eq!(s().col_type(ColType::Bytes), "BLOB");
    }

    #[test]
    fn autoincrement_uses_the_sqlite_spelling() {
        assert_eq!(
            s().autoincrement_ddl(ColType::I64),
            "INTEGER PRIMARY KEY AUTOINCREMENT"
        );
    }

    #[test]
    fn table_exists_uses_if_not_exists() {
        assert_eq!(
            s().table_exists_sql("users"),
            "CREATE TABLE IF NOT EXISTS \"users\""
        );
    }
}

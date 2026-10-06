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
        // 可更新列集为空（只有主键列的表）时 `DO UPDATE SET ` 是**空 SET**，
        // 真库语法错误。空集语义 = 「已存在就什么都不做」→ `DO NOTHING`。
        let action = if assigns.is_empty() {
            "DO NOTHING".to_string()
        } else {
            format!("DO UPDATE SET {assigns}")
        };
        format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) {action}",
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

    /// `UPDATE … FROM (VALUES …)`（SQLite 3.33+），但**不能**用其余四个方言共享的
    /// `update_many_from_values`：**SQLite 不接受表别名的列名清单**。
    ///
    /// 实测（sqlite3 3.46.1）：`… AS v("id", "name") WHERE …` 直接
    /// `near "(": syntax error`，而 `AS v` 则给 VALUES 的列按文档约定命名
    /// `column1…columnN`。所以这里按**位置**引用它们：第 1 列是主键、
    /// 接着是 `set_cols`、最后是 `where_extra`，与参数顺序一致。
    fn update_many_stmt(
        &self,
        table: &str,
        pk: &str,
        set_cols: &[String],
        where_extra: &[String],
        n_rows: usize,
    ) -> String {
        let per_row = 1 + set_cols.len() + where_extra.len();
        let rows = (0..n_rows)
            .map(|r| {
                let ph = (0..per_row)
                    .map(|c| self.placeholder(r * per_row + c + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({ph})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sets = set_cols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{} = v.{}",
                    self.quote(c),
                    self.quote(&format!("column{}", i + 2))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let q_table = self.quote(table);
        let mut conds = vec![format!(
            "{q_table}.{} = v.{}",
            self.quote(pk),
            self.quote("column1")
        )];
        for (i, c) in where_extra.iter().enumerate() {
            conds.push(format!(
                "{q_table}.{} = v.{}",
                self.quote(c),
                self.quote(&format!("column{}", set_cols.len() + i + 2))
            ));
        }
        format!(
            "UPDATE {q_table} SET {sets} FROM (VALUES {rows}) AS v WHERE {}",
            conds.join(" AND ")
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
        let l = s().limit_clause(Some(10), 0, false);
        assert_eq!(l.prefix, "");
        assert_eq!(l.suffix, " LIMIT 10");
    }

    #[test]
    fn limit_with_offset() {
        let l = s().limit_clause(Some(10), 20, false);
        assert_eq!(l.suffix, " LIMIT 10 OFFSET 20");
    }

    /// **只设 offset 不设 limit**（`Query::offset` 是公开方法，`.offset(n)` 合法）：
    /// `None` 时整个 `LIMIT` 子句必须**省略**。用 `u64::MAX` 顶替「未设置」
    /// 会产出 `LIMIT 18446744073709551615 OFFSET 20` —— 超出 BIGINT，真库拒收。
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

    /// 多行 UPDATE：SQLite 3.33+ 支持 `UPDATE … FROM (VALUES …)`，但
    /// **不接受表别名的列名清单** —— 实测 sqlite3 3.46.1：
    /// `… AS v("id", "name") WHERE …` 报 `near "(": syntax error`。
    /// 不写列名清单时 VALUES 的列按文档约定叫 `column1…columnN`，按位置引用。
    #[test]
    fn update_many_uses_from_values_with_positional_column_names() {
        let sql = s().update_many_stmt("docs", "id", &["name".into()], &[], 2);
        assert_eq!(
            sql,
            "UPDATE \"docs\" SET \"name\" = v.\"column2\" FROM (VALUES (?, ?), (?, ?)) \
             AS v WHERE \"docs\".\"id\" = v.\"column1\""
        );
    }

    /// 带 `where_extra`（乐观锁的旧版本号）时它排在主键与 SET 列之后 ——
    /// 位置算错就是把旧版本号当成了列值（静默错值）。
    #[test]
    fn update_many_puts_where_extras_after_the_set_columns() {
        let sql = s().update_many_stmt("docs", "id", &["name".into()], &["version".into()], 1);
        assert_eq!(
            sql,
            "UPDATE \"docs\" SET \"name\" = v.\"column2\" FROM (VALUES (?, ?, ?)) AS v \
             WHERE \"docs\".\"id\" = v.\"column1\" AND \"docs\".\"version\" = v.\"column3\""
        );
    }

    #[test]
    fn upsert_uses_on_conflict() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("ON CONFLICT"), "got: {sql}");
        assert!(sql.contains("DO UPDATE"), "got: {sql}");
    }

    /// **只有主键列时**（纯关联表、已读标记表）：`filter(|c| c != pk)` 之后
    /// 可更新列集为空，原实现产出 `DO UPDATE SET ` —— 空 SET，真库直接语法错误。
    /// 空集时的语义是「已存在就什么都不做」，即 `DO NOTHING`。
    #[test]
    fn upsert_with_only_the_pk_column_does_nothing() {
        let sql = s().upsert("users", &["id".into()], "id", 1);
        assert_eq!(
            sql,
            "INSERT INTO \"users\" (\"id\") VALUES (?) ON CONFLICT (\"id\") DO NOTHING"
        );
        assert!(!sql.contains("SET"), "不得留下空的 SET 片段: {sql}");
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

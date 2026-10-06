// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 方言差异层。**全部是纯函数**（输入 → 字符串），因此不需要数据库就能穷举
//! 所有分支 —— 而 CI 里只有 SQLite 能真跑。

use ecat_data::Dialect;

use crate::entity::ColType;

mod mssql;
mod mysql;
mod postgres;
mod sqlite;
mod standard;

/// 分页片段。**位置不唯一**：SQL Server 的 `TOP` 是 SELECT 前缀，
/// 其余方言的 `LIMIT/OFFSET` 是语句后缀。见「裁决 C」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    pub prefix: String,
    pub suffix: String,
}

impl Limit {
    pub fn none() -> Self {
        Self {
            prefix: String::new(),
            suffix: String::new(),
        }
    }
}

/// 主键回填方案。
///
/// `InsertThen` 存在的原因是 **MySQL 的 `LAST_INSERT_ID()` 是连接作用域的** ——
/// 在连接池下 `INSERT` 与 `SELECT LAST_INSERT_ID()` 是两次独立的池取用，可能落到
/// 不同连接，取回别的会话的值（**静默错值**）。因此 MySQL 必须把两条语句包进
/// 同一个事务以保证同连接（spec:585-589）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertPlan {
    /// 一步式：`INSERT … RETURNING pk` / `INSERT … OUTPUT INSERTED.pk`。
    Single { sql: String },
    /// 两步式：先 INSERT，再 `SELECT LAST_INSERT_ID()`。**两条必须在同一事务内**。
    InsertThen { insert: String, fetch: String },
}

pub trait DialectSpec: Send + Sync {
    /// 引用标识符。**输入必须已经过白名单校验**（见 `EntityMeta::column`）。
    fn quote(&self, ident: &str) -> String;

    /// 绑定占位符，1-based。`?` 类方言忽略下标。
    fn placeholder(&self, index: usize) -> String;

    fn limit_clause(&self, limit: u64, offset: u64, has_order: bool) -> Limit;

    fn insert_plan(&self, table: &str, cols: &[String], pk: &str, n_params: usize) -> InsertPlan;

    fn upsert(&self, table: &str, cols: &[String], pk: &str, n_params: usize) -> String;

    fn bool_literal(&self, b: bool) -> String;

    fn col_type(&self, ty: ColType) -> String;

    /// 建表语句的**开头部分**。SQLite/PG/MySQL 带 `IF NOT EXISTS`；
    /// SQL Server 没有该语法，改为返回空串并让调用方先查
    /// `INFORMATION_SCHEMA.TABLES`（见 `migrate::ddl`）。
    fn table_exists_sql(&self, table: &str) -> String;

    /// 自增主键列的完整 DDL 片段（含类型）。
    fn autoincrement_ddl(&self, ty: ColType) -> String;

    /// 单语句参数上限。批量操作据此分块（spec §5.5b）：
    /// **不分块 = 几千行批量插入必然报错。**
    fn max_params_per_stmt(&self) -> usize;
}

/// 按方言取实现。**穷举匹配，不用 `_ =>` 兜底** —— 新增方言变体时
/// 编译器会直接报错，而不是静默退回 Standard 生成错误 SQL。
pub fn lookup(d: Dialect) -> &'static dyn DialectSpec {
    match d {
        Dialect::Standard => &standard::StandardSpec,
        Dialect::Sqlite => &sqlite::SqliteSpec,
        Dialect::Postgres => &postgres::PostgresSpec,
        Dialect::MySql => &mysql::MySqlSpec,
        Dialect::Mssql => &mssql::MssqlSpec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_data::Dialect;

    #[test]
    fn lookup_covers_every_dialect_variant() {
        // 五个变体一个都不能漏 —— 漏了就是运行期 panic。
        for d in [
            Dialect::Standard,
            Dialect::Sqlite,
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::Mssql,
        ] {
            let spec = lookup(d);
            assert!(!spec.quote("x").is_empty(), "{d:?} 的 quote 返回了空串");
        }
    }

    /// 最容易犯的错：忘了给某个方言接线，它静默退回 Standard，于是生成
    /// `"col"` 而方言要 `[col]`、`FALSE` 而方言要 `1` —— 语法错误或静默错值。
    ///
    /// 本把守的取值必须与 Standard **不同**，否则就是退回。`quote` 两方言都是
    /// 双引号，区分不了，因此取 `bool_literal` 与 `max_params_per_stmt` 两条真有差异的轴。
    ///
    /// ⚠️ 本文件的 `lookup_covers_every_dialect_variant` 只查非空串，**拦不住退回** ——
    /// 真正拦退回的是下面按方言逐条挑轴的测试。
    #[test]
    fn sqlite_is_looked_up_not_falling_back_to_standard() {
        assert_eq!(lookup(Dialect::Standard).bool_literal(false), "FALSE");
        assert_eq!(lookup(Dialect::Standard).max_params_per_stmt(), 65535);
        assert_eq!(lookup(Dialect::Sqlite).bool_literal(false), "0");
        assert_eq!(lookup(Dialect::Sqlite).max_params_per_stmt(), 999);
    }

    /// `lookup` 的接线把守：Task 10 新接的三个方言必须真的被接上，
    /// 而不是静默退回 `Standard`。
    ///
    /// 挑轴原则：**必须与 Standard 的取值不同**，否则退回也测不出来。
    /// `quote` 在 Postgres 与 Standard 上都是双引号 —— 区分不了，故不用它。
    ///
    /// ⚠️ 这条把守**不可**用 `#[ignore]` 保留计划原文：本仓库零处 `#[ignore]`。
    #[test]
    fn non_standard_dialects_are_actually_wired_in_lookup() {
        // Postgres：占位符是 $n，Standard 是 ?
        assert_eq!(lookup(Dialect::Postgres).placeholder(1), "$1");
        // MySQL：引号是反引号，Standard 是双引号
        assert_eq!(lookup(Dialect::MySql).quote("id"), "`id`");
        // MSSQL：引号是方括号 + 参数上限 2100（Standard 是双引号 + 65535）
        assert_eq!(lookup(Dialect::Mssql).quote("id"), "[id]");
        assert_eq!(lookup(Dialect::Mssql).max_params_per_stmt(), 2100);
        // 反向对照：Standard 自己的值，证明上面几条不是恒真
        assert_eq!(lookup(Dialect::Standard).quote("id"), "\"id\"");
        assert_eq!(lookup(Dialect::Standard).max_params_per_stmt(), 65535);
    }

    /// SQL Server 的 2100 是最紧的，写错成 65535 会让批量插入在真库上炸。
    #[test]
    fn max_params_reflects_the_tightest_backend() {
        assert_eq!(lookup(Dialect::Sqlite).max_params_per_stmt(), 999);
        assert_eq!(lookup(Dialect::Standard).max_params_per_stmt(), 65535);
        assert_eq!(lookup(Dialect::Mssql).max_params_per_stmt(), 2100);
    }

    #[test]
    fn limit_none_is_empty_on_both_sides() {
        let l = Limit::none();
        assert!(l.prefix.is_empty());
        assert!(l.suffix.is_empty());
    }
}

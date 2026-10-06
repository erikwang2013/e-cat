// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `EntityMeta` → 建表 / 删表 DDL，以及「实体 → 迁移」的两个工厂。
//!
//! 类型映射**复用** [`DialectSpec::col_type`] / [`DialectSpec::autoincrement_ddl`]，
//! 这里不再写第二份映射表 —— 两份必然漂移，而漂移的表现是「某个方言上建出来的
//! 表类型和 CRUD 写进去的值对不上」。

use ecat_data::Dialect;

use super::MigrationBuilder;
use crate::dialect::{DialectSpec, lookup};
use crate::entity::{ColumnMeta, Entity, EntityMeta};
use crate::error::OrmError;

/// `EntityMeta` → `CREATE TABLE`。
///
/// 单列的形状是 `<引用名> <类型> [PRIMARY KEY | NOT NULL]`：
/// - 自增主键用 [`DialectSpec::autoincrement_ddl`]（它已含类型与方言拼法）
/// - 非自增主键用 [`DialectSpec::col_type`] + `PRIMARY KEY` —— **必须有**，
///   否则表建出来没主键，upsert / 乐观锁全部失去依据
/// - 非空的**非主键**列附 `NOT NULL`：漏掉它，「本不该为空」的列在数据库层就没有
///   约束，ORM 的 `UnexpectedNull` 成了唯一防线，而它只在读路径上
///
/// 主键列**不**重复写 `NOT NULL`：五个方言里主键都已隐含非空，而 `NOT NULL`
/// 写在 `PRIMARY KEY` 之后不是每个方言都接受的合法顺序（MySQL 对列属性顺序最严）。
/// ponytail: SQLite 的**非 INTEGER** 主键历史上允许 NULL（legacy quirk），
/// 要堵它得按方言特判 DDL；等真有非整数主键的真库用例再加。
///
/// 建表前缀来自 [`DialectSpec::create_table_prefix`] —— **MSSQL 上它没有
/// `IF NOT EXISTS` 但仍是完整前缀**，调用方要先按
/// [`DialectSpec::needs_exists_check_before_create`] 自查存在性（见
/// `version::ensure_version_table`）。
pub fn create_table_sql(meta: &EntityMeta, dialect: Dialect) -> String {
    let spec = lookup(dialect);
    let cols = meta
        .columns
        .iter()
        .map(|c| column_ddl(spec, c))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} ({cols})", spec.create_table_prefix(meta.table))
}

fn column_ddl(spec: &dyn DialectSpec, c: &ColumnMeta) -> String {
    let quoted = spec.quote(c.name);
    if c.pk && c.auto_increment {
        let ddl = spec.autoincrement_ddl(c.ty);
        // SQLite 的 `autoincrement_ddl` **自带** `PRIMARY KEY`
        // （`INTEGER PRIMARY KEY AUTOINCREMENT` 是它唯一合法的拼法），其余方言
        // 只给「类型 + 自增拼法」。无脑再补一次，SQLite 上就是**两个主键**，
        // 真库直接报 `table "x" has more than one primary key`（实测）。
        return if ddl.contains("PRIMARY KEY") {
            format!("{quoted} {ddl}")
        } else {
            format!("{quoted} {ddl} PRIMARY KEY")
        };
    }
    let ty = spec.col_type(c.ty);
    if c.pk {
        format!("{quoted} {ty} PRIMARY KEY")
    } else if c.nullable {
        format!("{quoted} {ty}")
    } else {
        format!("{quoted} {ty} NOT NULL")
    }
}

/// `EntityMeta` → `DROP TABLE`。**带 `IF NOT EXISTS`**：五个方言都支持它
/// （SQL Server 2016+ 亦然），而重跑迁移时表可能已经不在了。
pub fn drop_table_sql(meta: &EntityMeta, dialect: Dialect) -> String {
    format!("DROP TABLE IF EXISTS {}", lookup(dialect).quote(meta.table))
}

/// 从实体元数据造一条「建表」迁移。
///
/// **方言在 `run()` 时才知道**，所以这里只记实体与名字，SQL 延后到执行时按
/// 连接的 `dialect()` 生成 —— 否则迁移与连接串绑死，换库就要重写迁移列表。
pub fn create_table<E: Entity>() -> MigrationBuilder {
    MigrationBuilder::new(|d| create_table_sql(E::META, d))
}

/// `drop_table` 的实体工厂版。同 [`create_table`]：方言延后到执行时解析。
pub fn drop_table<E: Entity>() -> MigrationBuilder {
    MigrationBuilder::new(|d| drop_table_sql(E::META, d))
}

/// `"001_users"` → `1`：版本号取名字里**首个下划线之前的数字前缀**。
///
/// 解析失败时**报错**而不是静默用 0：版本号是迁移顺序的唯一依据，猜错会让迁移
/// 乱序执行（0 还会与「第 0 号迁移」撞车）。
pub(crate) fn parse_version(name: &str) -> Result<i64, OrmError> {
    let digits = name.split('_').next().unwrap_or_default();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(OrmError::InvalidMigrationName(name.to_string()));
    }
    digits
        .parse::<i64>()
        .map_err(|_| OrmError::InvalidMigrationName(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::*;
    use ecat_data::Dialect;

    static COLS: [ColumnMeta; 3] = [
        ColumnMeta {
            name: "id",
            ty: ColType::I64,
            nullable: false,
            pk: true,
            auto_increment: true,
        },
        ColumnMeta {
            name: "name",
            ty: ColType::Text,
            nullable: false,
            pk: false,
            auto_increment: false,
        },
        ColumnMeta {
            name: "bio",
            ty: ColType::Text,
            nullable: true,
            pk: false,
            auto_increment: false,
        },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users",
        pk: "id",
        columns: &COLS,
        relations: &[],
        flags: EntityFlags::NONE,
    };

    /// **整串断言** —— 「含 CREATE TABLE 关键字」这种查法对「列顺序错位」
    /// 「引号用错方言」全都恒真。
    #[test]
    fn create_table_is_an_exact_statement_on_postgres() {
        assert_eq!(
            create_table_sql(&META, Dialect::Postgres),
            "CREATE TABLE IF NOT EXISTS \"users\" (\"id\" BIGSERIAL PRIMARY KEY, \
             \"name\" TEXT NOT NULL, \"bio\" TEXT)"
        );
    }

    /// 可空列写 `NULL`、非空列写 `NOT NULL` —— 漏掉 NOT NULL 会让
    /// 「本不该为空的列」在数据库层没有约束，ORM 的 `UnexpectedNull` 报错
    /// 就成了唯一防线（而它只在读路径上）。
    #[test]
    fn nullability_is_emitted_explicitly() {
        let sql = create_table_sql(&META, Dialect::Postgres);
        assert!(!sql.contains("\"bio\" TEXT NOT NULL"), "bio 可空: {sql}");
        assert!(sql.contains("\"name\" TEXT NOT NULL"), "name 非空: {sql}");
    }

    #[test]
    fn autoincrement_pk_uses_the_dialect_spelling() {
        let pg = create_table_sql(&META, Dialect::Postgres);
        assert!(pg.contains("\"id\" BIGSERIAL PRIMARY KEY"), "got: {pg}");
        let my = create_table_sql(&META, Dialect::MySql);
        assert!(
            my.contains("`id` BIGINT AUTO_INCREMENT PRIMARY KEY"),
            "got: {my}"
        );
        let ms = create_table_sql(&META, Dialect::Mssql);
        assert!(
            ms.contains("[id] BIGINT IDENTITY(1,1) PRIMARY KEY"),
            "got: {ms}"
        );
        let lite = create_table_sql(&META, Dialect::Sqlite);
        assert!(
            lite.contains("\"id\" INTEGER PRIMARY KEY AUTOINCREMENT"),
            "got: {lite}"
        );
    }

    /// **自增主键只能声明一次**：SQLite 的 `autoincrement_ddl` 自己就带
    /// `PRIMARY KEY`，再补一个真库直接拒绝
    /// （实测 `table "users" has more than one primary key`）。
    #[test]
    fn autoincrement_pk_declares_primary_key_exactly_once() {
        for d in [
            Dialect::Standard,
            Dialect::Sqlite,
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::Mssql,
        ] {
            let sql = create_table_sql(&META, d);
            assert_eq!(
                sql.matches("PRIMARY KEY").count(),
                1,
                "{d:?} 上主键应恰好声明一次: {sql}"
            );
        }
    }

    /// SQLite 的整串形态 —— 它的自增拼法与其余四个都不同（`INTEGER PRIMARY KEY
    /// AUTOINCREMENT` 是唯一合法的写法，换成 BIGINT 就不再是 rowid 别名）。
    #[test]
    fn create_table_is_an_exact_statement_on_sqlite() {
        assert_eq!(
            create_table_sql(&META, Dialect::Sqlite),
            "CREATE TABLE IF NOT EXISTS \"users\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \
             \"name\" TEXT NOT NULL, \"bio\" TEXT)"
        );
    }

    /// 非自增主键也要写 PRIMARY KEY —— 否则表建出来没有主键，
    /// 后续的 upsert / 乐观锁全部失去依据。
    #[test]
    fn non_autoincrement_pk_still_gets_a_primary_key_clause() {
        static COLS2: [ColumnMeta; 1] = [ColumnMeta {
            name: "id",
            ty: ColType::Text,
            nullable: false,
            pk: true,
            auto_increment: false,
        }];
        static META2: EntityMeta = EntityMeta {
            table: "k",
            pk: "id",
            columns: &COLS2,
            relations: &[],
            flags: EntityFlags::NONE,
        };
        let sql = create_table_sql(&META2, Dialect::Postgres);
        assert!(sql.contains("PRIMARY KEY"), "got: {sql}");
        assert!(!sql.contains("BIGSERIAL"), "非自增不该有 BIGSERIAL: {sql}");
    }

    /// MSSQL 没有 `CREATE TABLE IF NOT EXISTS` —— 调用方先查
    /// INFORMATION_SCHEMA。本函数生成的 SQL **不含** IF NOT EXISTS。
    #[test]
    fn mssql_create_table_has_no_if_not_exists() {
        let sql = create_table_sql(&META, Dialect::Mssql);
        assert!(!sql.contains("IF NOT EXISTS"), "got: {sql}");
    }

    #[test]
    fn drop_table_is_dialect_quoted() {
        assert_eq!(
            drop_table_sql(&META, Dialect::MySql),
            "DROP TABLE IF EXISTS `users`"
        );
        assert_eq!(
            drop_table_sql(&META, Dialect::Mssql),
            "DROP TABLE IF EXISTS [users]"
        );
        assert_eq!(
            drop_table_sql(&META, Dialect::Postgres),
            "DROP TABLE IF EXISTS \"users\""
        );
    }
}

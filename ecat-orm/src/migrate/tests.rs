// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `migrate` 的纯函数断言。**单独成文件**是为了守住「每个源文件 < 500 行」，
//! 与 `query/sql_tests.rs` / `crud/tests.rs` 同款。

use super::*;
use crate::entity::Entity;
use crate::error::OrmError;
use ecat_data::Dialect;

#[test]
fn status_classifies_applied_and_pending() {
    let s = classify(&[1, 2], &[1, 2, 3, 4]);
    assert_eq!(s.applied, vec![1, 2]);
    assert_eq!(s.pending, vec![3, 4]);
}

/// 已应用的版本里有、但迁移列表里没有的（例如分支回滚后遗留）——
/// 必须**报出来**而不是静默忽略：那说明数据库状态和代码不一致。
#[test]
fn unknown_applied_versions_are_surfaced() {
    let s = classify(&[1, 2, 9], &[1, 2]);
    assert_eq!(s.unknown, vec![9]);
    assert_eq!(s.applied, vec![1, 2]);
}

/// 待应用项**按版本号排序**，不按声明顺序 —— 声明顺序在多次编辑后
/// 很容易与实际版本号不符。
#[test]
fn pending_is_sorted_by_version_not_declaration_order() {
    let s = classify(&[], &[3, 1, 2]);
    assert_eq!(s.pending, vec![1, 2, 3]);
}

/// 版本表里的顺序也不作数：`applied` 同样按版本号升序。
#[test]
fn applied_is_sorted_too() {
    let s = classify(&[3, 1], &[1, 2, 3]);
    assert_eq!(s.applied, vec![1, 3]);
    assert_eq!(s.pending, vec![2]);
}

#[test]
fn down_without_reverse_sql_is_irreversible() {
    let m = Migration::new(1, "001_users".into(), "CREATE TABLE t (a int)".into());
    assert!(m.reverse_sql.is_none());
    let e = m.reverse().unwrap_err();
    assert!(
        matches!(e, OrmError::MigrationIrreversible(_)),
        "got: {e:?}"
    );
}

#[test]
fn down_with_reverse_sql_is_allowed() {
    let mut m = Migration::new(1, "001_users".into(), "CREATE TABLE t (a int)".into());
    m.reverse_sql = Some("DROP TABLE t".into());
    assert_eq!(m.reverse().unwrap(), "DROP TABLE t");
}

/// 版本号取名字里**首个下划线之前的数字前缀**，前导零不影响数值。
#[test]
fn parse_version_reads_the_numeric_prefix() {
    assert_eq!(parse_version("001_users").unwrap(), 1);
    assert_eq!(parse_version("002_posts").unwrap(), 2);
    assert_eq!(parse_version("010_x").unwrap(), 10);
    assert_eq!(parse_version("7_no_padding").unwrap(), 7);
}

/// 解析失败必须**报错**，不能静默用 0 —— 版本号是迁移顺序的唯一依据，
/// 猜错会让迁移乱序执行（而且 0 会与「第 0 号迁移」撞车）。
#[test]
fn parse_version_refuses_names_without_a_numeric_prefix() {
    for bad in [
        "users",
        "",
        "_001_users",
        "v1_users",
        "1x_users",
        // 溢出 i64：不能 wrap、也不能当 0
        "99999999999999999999_users",
    ] {
        let r = parse_version(bad);
        assert!(
            matches!(r, Err(OrmError::InvalidMigrationName(_))),
            "{bad:?} 应报错，得到 {r:?}"
        );
    }
    // 没有下划线时整个名字就是前缀 —— 纯数字名字合法
    assert_eq!(parse_version("001").unwrap(), 1);
}

/// **方言在 `run()` 时才知道**：同一个工厂构造出的构建器，按不同方言
/// 解析出不同 SQL。这条把「迁移与连接串绑死」这个退化挡在门外。
#[test]
fn a_factory_resolves_the_dialect_when_built_not_when_declared() {
    let b = crate::migrate::ddl::create_table::<crate::crud::fixtures::U>();
    let pg = b.build(1, "001_users".into(), Dialect::Postgres);
    let ms = b.build(1, "001_users".into(), Dialect::Mssql);
    assert!(
        pg.sql.starts_with("CREATE TABLE IF NOT EXISTS \"users\""),
        "got: {}",
        pg.sql
    );
    assert!(
        ms.sql.starts_with("CREATE TABLE [users]"),
        "got: {}",
        ms.sql
    );
    assert_ne!(pg.sql, ms.sql, "两个方言不该产出同一条 SQL");
    assert!(pg.reverse_sql.is_none(), "没给反向 SQL 就不该凭空造一条");
}

/// `with_reverse` 的反向 SQL 同样按方言解析。
#[test]
fn with_reverse_is_resolved_per_dialect() {
    let b = MigrationBuilder::new(|d| drop_table_sql(crate::crud::fixtures::U::META, d))
        .with_reverse(|d| drop_table_sql(crate::crud::fixtures::U::META, d));
    let m = b.build(1, "001_x".into(), Dialect::MySql);
    assert_eq!(m.reverse().unwrap(), "DROP TABLE IF EXISTS `users`");
}

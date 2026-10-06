// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `#[derive(Entity)]` 的列字段路径（不含关联，关联见 derive_relations.rs）。

use ecat_orm::{ColType, Entity, OrmError, Row};
use time::OffsetDateTime;

#[derive(Entity, Debug, PartialEq)]
#[entity(table = "users")]
pub struct User {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub score: f64,
    pub active: bool,
    #[entity(created_at)]
    pub created_at: Option<OffsetDateTime>,
    #[entity(version)]
    pub version: i64,
}

/// 表名省略时取结构体名的 snake_case。
#[derive(Entity)]
pub struct UserProfile {
    #[entity(pk)]
    pub id: i64,
}

fn row(cols: &[&str], vals: Vec<serde_json::Value>) -> Row {
    Row::new(cols.iter().map(|s| s.to_string()).collect(), vals)
}

#[test]
fn table_name_from_attribute() {
    assert_eq!(User::TABLE, "users");
    assert_eq!(User::PK, "id");
}

#[test]
fn table_name_defaults_to_snake_case_of_struct() {
    assert_eq!(UserProfile::TABLE, "user_profile");
}

#[test]
fn columns_are_declared_in_field_order() {
    let names: Vec<_> = User::META.columns.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        vec![
            "id",
            "name",
            "email",
            "score",
            "active",
            "created_at",
            "version"
        ]
    );
}

#[test]
fn column_types_come_from_the_value_impls() {
    let ty = |n: &str| User::META.column(n).unwrap().ty;
    assert_eq!(ty("id"), ColType::I64);
    assert_eq!(ty("name"), ColType::Text);
    assert_eq!(ty("email"), ColType::Text); // Option<String> → String 的列类型
    assert_eq!(ty("score"), ColType::F64);
    assert_eq!(ty("active"), ColType::Bool);
    assert_eq!(ty("created_at"), ColType::Timestamp);
}

#[test]
fn nullability_follows_the_option_wrapper() {
    let nullable = |n: &str| User::META.column(n).unwrap().nullable;
    assert!(!nullable("name"));
    assert!(nullable("email"));
    assert!(nullable("created_at"));
}

#[test]
fn pk_and_auto_increment_flags() {
    let id = User::META.column("id").unwrap();
    assert!(id.pk);
    assert!(id.auto_increment);
    assert!(!User::META.column("name").unwrap().pk);
}

#[test]
fn flags_point_at_the_right_columns() {
    assert_eq!(User::META.flags.created_at, Some("created_at"));
    assert_eq!(User::META.flags.version, Some("version"));
    assert_eq!(User::META.flags.updated_at, None);
    assert_eq!(User::META.flags.soft_delete, None);
}

#[test]
fn insertable_columns_skip_the_auto_increment_pk() {
    let names: Vec<_> = User::META.insertable_columns().map(|c| c.name).collect();
    assert!(!names.contains(&"id"), "自增主键不该出现在 INSERT 里");
    assert!(names.contains(&"name"));
}

#[test]
fn updatable_columns_skip_the_pk() {
    let names: Vec<_> = User::META.updatable_columns().map(|c| c.name).collect();
    assert!(!names.contains(&"id"));
    assert!(names.contains(&"version"));
}

#[test]
fn from_row_builds_the_struct() {
    let r = row(
        &[
            "id",
            "name",
            "email",
            "score",
            "active",
            "created_at",
            "version",
        ],
        vec![
            serde_json::json!(1),
            serde_json::json!("alice"),
            serde_json::json!(null),
            serde_json::json!(9.5),
            serde_json::json!(true),
            serde_json::json!(null),
            serde_json::json!(3),
        ],
    );
    let u = User::from_row(&r).unwrap();
    assert_eq!(u.id, 1);
    assert_eq!(u.name, "alice");
    assert_eq!(u.email, None);
    assert_eq!(u.score, 9.5);
    assert!(u.active);
    assert_eq!(u.version, 3);
}

/// 非 Option 字段遇到 NULL 必须报错，不得静默取默认值。
#[test]
fn from_row_rejects_null_for_non_optional_field() {
    let r = row(
        &[
            "id",
            "name",
            "email",
            "score",
            "active",
            "created_at",
            "version",
        ],
        vec![
            serde_json::json!(1),
            serde_json::json!(null), // name 非 Option
            serde_json::json!(null),
            serde_json::json!(1.0),
            serde_json::json!(false),
            serde_json::json!(null),
            serde_json::json!(0),
        ],
    );
    let e = User::from_row(&r).unwrap_err();
    assert!(
        matches!(e, OrmError::UnexpectedNull { column: "name" }),
        "got: {e:?}"
    );
}

#[test]
fn to_values_returns_every_column_in_meta_order() {
    let u = User {
        id: 1,
        name: "alice".into(),
        email: None,
        score: 9.5,
        active: true,
        created_at: None,
        version: 3,
    };
    let vals = u.to_values();
    let names: Vec<_> = vals.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        vec![
            "id",
            "name",
            "email",
            "score",
            "active",
            "created_at",
            "version"
        ]
    );
    assert_eq!(vals[1].1, serde_json::json!("alice"));
    assert_eq!(vals[2].1, serde_json::json!(null));
}

#[test]
fn pk_value_returns_the_pk_field() {
    let u = User {
        id: 42,
        name: "alice".into(),
        email: None,
        score: 0.0,
        active: false,
        created_at: None,
        version: 0,
    };
    assert_eq!(u.pk_value(), serde_json::json!(42));
}

/// `OffsetDateTime` 必须是 RFC3339 字符串 —— 走 `time` 的 serde 会变成数组。
#[test]
fn timestamp_serializes_as_rfc3339() {
    let u = User {
        id: 1,
        name: "a".into(),
        email: None,
        score: 0.0,
        active: false,
        created_at: Some(time::macros::datetime!(2026-10-05 04:00:00 UTC)),
        version: 0,
    };
    let (_, v) = u
        .to_values()
        .into_iter()
        .find(|(n, _)| *n == "created_at")
        .unwrap();
    assert_eq!(v, serde_json::json!("2026-10-05T04:00:00Z"));
}

/// 覆盖 `column = "..."` 重命名与 `updated_at` / `soft_delete` 标志位。
///
/// **补测理由**（Task 7 实施者的空验收探针发现）：
/// - `column` 属性在原 14 个测试里**零覆盖**
/// - `updated_at` / `soft_delete` 只被断言为 `None` —— 那是**恒真**的，
///   测不出「没接线」与「接线了但恰好是 None」的区别
///
/// 探针当时验证过它们能工作，但探针未进提交，洞就还在。
#[derive(Entity, Debug, PartialEq)]
#[entity(table = "articles")]
pub struct Article {
    #[entity(pk, auto_increment)]
    pub id: i64,
    #[entity(column = "headline")]
    pub title: String,
    #[entity(updated_at)]
    pub touched_at: Option<OffsetDateTime>,
    #[entity(soft_delete)]
    pub removed_at: Option<OffsetDateTime>,
}

#[test]
fn column_attribute_renames_the_column() {
    let names: Vec<_> = Article::META.columns.iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["id", "headline", "touched_at", "removed_at"]);
    assert!(Article::META.column("headline").is_some());
    assert!(
        Article::META.column("title").is_none(),
        "字段名不该出现在列名里 —— 重命名没生效"
    );
}

/// 重命名必须同时作用于**读写两个方向**：只改元数据不改 `from_row`/`to_values`
/// 的话，SQL 用 `headline` 而取值用 `title`，会静默报 UnknownColumn。
#[test]
fn renamed_column_roundtrips_through_row_and_values() {
    let r = row(
        &["id", "headline", "touched_at", "removed_at"],
        vec![
            serde_json::json!(1),
            serde_json::json!("hello"),
            serde_json::json!(null),
            serde_json::json!(null),
        ],
    );
    let a = Article::from_row(&r).unwrap();
    assert_eq!(a.title, "hello");

    let vals = a.to_values();
    assert!(
        vals.iter()
            .any(|(n, v)| *n == "headline" && v == &serde_json::json!("hello")),
        "to_values 必须用重命名后的列名，得到: {vals:?}"
    );
    assert!(
        !vals.iter().any(|(n, _)| *n == "title"),
        "to_values 不得出现字段名: {vals:?}"
    );
}

#[test]
fn updated_at_and_soft_delete_flags_point_at_the_marked_columns() {
    assert_eq!(
        Article::META.flags.updated_at,
        Some("touched_at"),
        "updated_at 必须指向被标记的那一列（字段名，非类型名）"
    );
    assert_eq!(Article::META.flags.soft_delete, Some("removed_at"));
    // 对照：User 没标记这两者，应为 None —— 这条让上面两条不再是恒真断言
    assert_eq!(User::META.flags.updated_at, None);
    assert_eq!(User::META.flags.soft_delete, None);
}

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

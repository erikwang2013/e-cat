// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 查询构建器本体：白名单、类型状态、操作符。从 `query/mod.rs` 搬出来 ——
//! 合在一起会顶过「每个源文件 < 500 行」的硬规则。
//! 删除路径见 `query/delete_tests.rs`，SQL 生成见 `query/sql_tests.rs`。

use super::*;
use crate::entity::*;
use serde_json::json;

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
        name: "age",
        ty: ColType::I64,
        nullable: false,
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

#[derive(Debug)]
struct U;
impl Entity for U {
    const TABLE: &'static str = "users";
    const PK: &'static str = "id";
    const META: &'static EntityMeta = &META;
    fn from_row(_r: &ecat_data::Row) -> Result<Self, OrmError> {
        Ok(U)
    }
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
        vec![]
    }
    fn pk_value(&self) -> serde_json::Value {
        json!(0)
    }
    fn set_relation(&mut self, _name: &str, _rows: Vec<ecat_data::Row>) -> Result<(), OrmError> {
        Ok(())
    }
}

// ---- 白名单 ----

#[test]
fn known_column_is_accepted() {
    assert!(U::query().filter("name", Op::Eq, "x").is_ok());
}

/// **安全约束**：未声明的列名必须被拒，且**不得出现在生成的 SQL 里**。
#[test]
fn unknown_column_is_rejected() {
    let e = U::query().filter("naem", Op::Eq, "x").unwrap_err();
    assert!(
        matches!(e, OrmError::UnknownColumn(ref c) if c == "naem"),
        "got: {e:?}"
    );
}

/// 注入尝试必须被白名单拦下 —— 它不在 columns 里。
#[test]
fn injection_attempt_is_rejected_as_unknown_column() {
    let e = U::query()
        .filter("name\" = 'x' OR 1=1 --", Op::Eq, "x")
        .unwrap_err();
    assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
}

#[test]
fn unknown_order_by_column_is_rejected() {
    let e = U::query().order_by("nope", Order::Asc).unwrap_err();
    assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
}

// ---- 类型状态 ----

#[test]
fn filter_transitions_to_filtered() {
    let q: Query<U, Filtered> = U::query().filter("name", Op::Eq, "x").unwrap();
    assert_eq!(q.filter_count(), 1);
}

#[test]
fn chaining_filters_accumulates() {
    let q = U::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .filter("age", Op::Gt, 18)
        .unwrap();
    assert_eq!(q.filter_count(), 2);
}

// ---- 操作符 ----

#[test]
fn in_requires_an_array_value() {
    let ok = U::query().filter("id", Op::In, json!([1, 2, 3]));
    assert!(ok.is_ok());
    let bad = U::query().filter("id", Op::In, json!(1));
    assert!(
        matches!(bad, Err(OrmError::Rdbms(_))),
        "In 传非数组必须报错"
    );
}

#[test]
fn is_null_ignores_the_value() {
    let q = U::query()
        .filter("name", Op::IsNull, json!("whatever"))
        .unwrap();
    assert_eq!(q.filter_count(), 1);
}

// ---- 裁决 B：with_trashed 在两个状态上都可用 ----

#[test]
fn with_trashed_works_before_and_after_filter() {
    let a = U::query().with_trashed().filter("id", Op::Eq, 1).unwrap();
    let b = U::query().filter("id", Op::Eq, 1).unwrap().with_trashed();
    assert!(a.is_with_trashed() && b.is_with_trashed());
}

// ---- 逃生口 ----

#[test]
fn filter_raw_bypasses_the_whitelist_by_design() {
    // 关联表的列走这里。文档必须写明「输入必须可信」。
    let q = U::query().filter_raw("posts.published = 1").unwrap();
    assert_eq!(q.filter_count(), 1);
}

/// **`filter_raw` 必须把状态转成 `Filtered`** —— 类型状态的判据是
/// 「已经有一个过滤条件」，而它正是加了一个。
///
/// 不转的后果：只用逃生口写条件的查询永远进不了删改状态，
/// 用户得再加一个 dummy `.filter(..)` 才能调 `delete_where`。
///
/// 本测试用**类型标注**钉住返回状态：`filter_raw` 若退回 `Self`，
/// 这里会因为 `Query<U, Unfiltered>` 与 `Query<U, Filtered>` 不符而编译失败。
#[test]
fn filter_raw_transitions_to_filtered() {
    let q: Query<U, Filtered> = U::query().filter_raw("posts.published = 1").unwrap();
    assert_eq!(q.filter_count(), 1);
}

#[test]
fn order_by_accepts_known_column_and_direction() {
    let q = U::query().order_by("id", Order::Desc).unwrap();
    assert_eq!(q.order_count(), 1);
}

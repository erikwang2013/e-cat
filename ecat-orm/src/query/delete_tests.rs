// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `Query::delete_where` / `hard_delete_where` —— 它们是
//! `batch::delete_where` 的薄封装。这里只验「类型状态与 `with_trashed`
//! 的翻译」这一层：会不会传错 `hard`、读取开关会不会漏进删改路径。
//! 分块与 WHERE 本身见 `batch/delete_tests.rs`。

use super::*;
use crate::crud::auto_tests::D;
use crate::crud::fixtures::*;
use crate::entity::Entity;
use ecat_data::Dialect;
use serde_json::json;

/// **`with_trashed()` 是读取开关，删改路径上无效**：加不加，语句必须逐字符相同。
///
/// 软删除实体的闸门也照旧 —— 若把 `with_trashed` 当成「连已软删的一起处理」，
/// 这里的 `IS NULL` 会消失，而那是「重复删除刷新删除时刻」的入口。
#[tokio::test]
async fn with_trashed_does_not_change_the_delete_path() {
    let plain = spy(Dialect::Sqlite, 1);
    let trashed = spy(Dialect::Sqlite, 1);

    let a = D::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .delete_where(&plain)
        .await
        .unwrap();
    let b = D::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .with_trashed()
        .delete_where(&trashed)
        .await
        .unwrap();

    assert_eq!(a, b, "受影响行数也要一样");
    assert_eq!(
        last(&plain).0,
        r#"UPDATE "docs" SET "deleted_at" = ? WHERE "deleted_at" IS NULL AND "name" = ?"#
    );
    assert_eq!(last(&plain).0, last(&trashed).0, "语句必须逐字符相同");
}

/// 硬删除入口必须把闸门关掉 —— 传成 `hard = false` 会让「物理删除」变成
/// 又软删一次（行还在，且调用方以为删干净了）。
#[tokio::test]
async fn hard_delete_where_bypasses_the_gate() {
    let s = spy(Dialect::Sqlite, 3);
    let n = D::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .hard_delete_where(&s)
        .await
        .unwrap();
    assert_eq!(n, 3);
    assert_eq!(last(&s).0, r#"DELETE FROM "docs" WHERE "name" = ?"#);
}

/// 没有 `soft_delete` 的实体两条路都是 `DELETE FROM`（对照：上面两条的
/// 差异全部由 `flags.soft_delete` 驱动，不是「delete_where 永远发 UPDATE」）。
#[tokio::test]
async fn a_plain_entity_is_deleted_with_delete_from_on_both_paths() {
    for hard in [false, true] {
        let s = spy(Dialect::Sqlite, 1);
        let q = U::query().filter("id", Op::Eq, 1).unwrap();
        let n = if hard {
            q.hard_delete_where(&s).await.unwrap()
        } else {
            q.delete_where(&s).await.unwrap()
        };
        assert_eq!(n, 1);
        assert_eq!(last(&s).0, r#"DELETE FROM "users" WHERE "id" = ?"#);
    }
}

/// spec §5.4 的用法：`User::query().filter(..).delete_where(&db)`。
/// `Op::In` 在上限之内时**只有一条**语句 —— 分块逻辑不得把能一次发的拆碎。
#[tokio::test]
async fn an_in_filter_within_the_limit_stays_one_statement() {
    let s = spy(Dialect::Sqlite, 1);
    let ids: Vec<serde_json::Value> = (0..100).map(|i| json!(i)).collect();
    let n = U::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .filter("id", Op::In, json!(ids))
        .unwrap()
        .delete_where(&s)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let calls = s.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.len(), 101);
    assert!(
        calls[0]
            .0
            .starts_with(r#"DELETE FROM "users" WHERE "name" = ? AND "id" IN ("#),
        "got: {}",
        calls[0].0
    );
}

/// 逃生口写出来的条件同样能删 —— `filter_raw` 把状态转成 `Filtered` 的
/// 全部意义就在这里（不然用户得再加一个 dummy `.filter(..)`）。
#[tokio::test]
async fn the_raw_escape_hatch_can_delete_too() {
    let s = spy(Dialect::Sqlite, 4);
    let n = U::query()
        .filter_raw("age < 18")
        .unwrap()
        .hard_delete_where(&s)
        .await
        .unwrap();
    assert_eq!(n, 4);
    assert_eq!(last(&s).0, r#"DELETE FROM "users" WHERE age < 18"#);
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `delete_where` / 硬删除的批量面。
//!
//! 这里有**两个**入口：`batch::delete_where(.., hard)` 是唯一的实现，
//! `Query::delete_where` / `Query::hard_delete_where` 只是转发（见
//! `query/builder_tests.rs`）。WHERE 渲染复用 `query::sql::render_where` ——
//! 两份 WHERE 渲染必然漂移，而那里是白名单安全边界。

use super::*;
use crate::crud::auto_tests::{D, assert_filled_now};
use crate::crud::fixtures::*;
use crate::query::{Expr, Op};
use ecat_data::{Dialect, SqlExecutor};
use serde_json::json;

/// 无 `soft_delete` 的实体 → 就是一条 `DELETE FROM`。
#[tokio::test]
async fn a_plain_entity_is_deleted_with_delete_from() {
    let s = spy(Dialect::Sqlite, 3);
    let f = vec![Expr::Cmp {
        column: "id".into(),
        op: Op::Eq,
        value: json!(1),
    }];
    assert_eq!(delete_where(&s, U::META, &f, true).await.unwrap(), 3);
    let (sql, params) = last(&s);
    assert_eq!(sql, r#"DELETE FROM "users" WHERE "id" = ?"#);
    assert_eq!(params, vec![json!(1)]);
}

/// 带 `soft_delete` 的实体 → `UPDATE … SET <sd> = ?`，且**带 `IS NULL` 闸门**
/// （与 `delete_by_id` 同语义：闸门让重复删除影响 0 行，而不是把删除时刻刷新）。
#[tokio::test]
async fn a_soft_delete_entity_is_updated_with_the_is_null_gate() {
    let s = spy(Dialect::Sqlite, 1);
    let f = vec![Expr::Cmp {
        column: "name".into(),
        op: Op::Eq,
        value: json!("x"),
    }];
    assert_eq!(delete_where(&s, D::META, &f, false).await.unwrap(), 1);
    let (sql, params) = last(&s);
    assert_eq!(
        sql,
        r#"UPDATE "docs" SET "deleted_at" = ? WHERE "deleted_at" IS NULL AND "name" = ?"#
    );
    assert_filled_now(&params[0]);
    assert_eq!(params[1], json!("x"));
}

/// **硬删除必须关掉软删除闸门**：不然已经软删的行（`deleted_at IS NOT NULL`）
/// 会被留在表里 —— 它们既查不到、又删不掉，永久占着。
#[tokio::test]
async fn hard_delete_ignores_the_soft_delete_gate() {
    let s = spy(Dialect::Sqlite, 2);
    let f = vec![Expr::Cmp {
        column: "name".into(),
        op: Op::Eq,
        value: json!("x"),
    }];
    assert_eq!(delete_where(&s, D::META, &f, true).await.unwrap(), 2);
    let (sql, params) = last(&s);
    assert_eq!(sql, r#"DELETE FROM "docs" WHERE "name" = ?"#);
    assert_eq!(params, vec![json!("x")]);
}

/// `Op::In` 超过上限：切成多条，受影响行数**累加**。
/// SQLite 上限 999 → 1000 个值切 `[999, 1]`。
#[tokio::test]
async fn in_filters_split_over_the_param_limit_and_sum_affected_rows() {
    let s = spy(Dialect::Sqlite, 1);
    let f = vec![Expr::In {
        column: "id".into(),
        values: (0..1000).map(|i| json!(i)).collect(),
        negated: false,
    }];
    assert_eq!(
        delete_where(&s, U::META, &f, true).await.unwrap(),
        2,
        "两块各影响 1 行，必须累加"
    );

    let calls = s.calls();
    assert_eq!(calls.len(), 2, "1000 个值 @999 必须切成两块");
    assert_eq!(calls[0].1.len(), 999);
    assert_eq!(calls[1].1.len(), 1);
    assert_eq!(calls[0].0.matches("?").count(), 999);
    assert_eq!(calls[1].0, r#"DELETE FROM "users" WHERE "id" IN (?)"#);
}

/// 分块时**非 In 的条件必须留在每一条语句里** —— 只带上 IN 会删掉范围外的行
/// （数据丢失，且不报错）。
#[tokio::test]
async fn other_filters_survive_every_split_statement() {
    let s = spy(Dialect::Sqlite, 1);
    let f = vec![
        Expr::Cmp {
            column: "name".into(),
            op: Op::Eq,
            value: json!("x"),
        },
        Expr::In {
            column: "id".into(),
            values: (0..1000).map(|i| json!(i)).collect(),
            negated: false,
        },
    ];
    assert_eq!(delete_where(&s, U::META, &f, true).await.unwrap(), 2);

    let calls = s.calls();
    assert_eq!(calls.len(), 2);
    // 预算 = 999 − 1（name 的参数）= 998 → 每块 [998, 2] 个值。
    assert_eq!(calls[0].1.len(), 999);
    assert_eq!(calls[1].1.len(), 3);
    for (sql, params) in &calls {
        assert!(
            sql.starts_with(r#"DELETE FROM "users" WHERE "name" = ? AND "id" IN ("#),
            "got: {sql}"
        );
        assert_eq!(params[0], json!("x"), "非 In 条件必须在每一条里: {sql}");
    }
}

/// **空的 `IN` 仍然要发一条语句**。空列表在 `render_where` 里是 `1 = 0`
/// （`IN ()` 是语法错误）；而 `chunks()` 在空切片上产出**零块** ——
/// 直接拿零块做乘积会让整条删除一条语句都不发（静默什么都不做，看起来像成功）。
#[tokio::test]
async fn an_empty_in_filter_still_issues_one_statement() {
    let s = spy(Dialect::Sqlite, 0);
    let f = vec![Expr::In {
        column: "id".into(),
        values: vec![],
        negated: false,
    }];
    assert_eq!(delete_where(&s, U::META, &f, true).await.unwrap(), 0);
    let calls = s.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, r#"DELETE FROM "users" WHERE 1 = 0"#);
}

/// 多列同时分块：两个 `IN` 的取值范围必须做**笛卡尔积**，
/// 否则 (a 块, b 块) 的组合会被漏掉 —— 静默少删。
#[tokio::test]
async fn two_in_filters_are_cross_produced() {
    let s = spy(Dialect::Sqlite, 1);
    let f = vec![
        Expr::In {
            column: "id".into(),
            values: (0..600).map(|i| json!(i)).collect(),
            negated: false,
        },
        Expr::In {
            column: "name".into(),
            values: (0..600).map(|i| json!(format!("n{i}"))).collect(),
            negated: false,
        },
    ];
    // 预算 999 / 2 个 In = 每块 499 个值 → 600 = [499, 101] → 2×2 = 4 条。
    assert_eq!(delete_where(&s, U::META, &f, true).await.unwrap(), 4);
    let calls = s.calls();
    assert_eq!(calls.len(), 4, "两个 In 各自分块后要交叉组合");
    for (_, params) in &calls {
        assert!(params.len() <= 999, "每条语句都不得超限");
    }
    // 覆盖性：两个过滤器的取值各自被**完整**覆盖（笛卡尔积漏掉组合就会少值）。
    // 用类型分区：id 是数字、name 是字符串。
    let mut ids = std::collections::BTreeSet::new();
    let mut names = std::collections::BTreeSet::new();
    for (_, params) in &calls {
        for v in params {
            if let Some(n) = v.as_i64() {
                ids.insert(n);
            } else if let Some(s) = v.as_str() {
                names.insert(s.to_string());
            }
        }
    }
    assert_eq!(ids.len(), 600, "id 的 600 个取值必须被完整覆盖");
    assert_eq!(names.len(), 600, "name 的 600 个取值必须被完整覆盖");
}

/// 真库上跑通：软删除真的把行藏起来、硬删除真的把它送走。
#[tokio::test]
async fn delete_where_round_trips_on_a_real_database() {
    let db = mem_sqlite("delete_where").await;
    db.execute(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
         created_at TEXT, updated_at TEXT, deleted_at TEXT, version INTEGER NOT NULL)",
    )
    .await
    .unwrap();
    for _ in 0..3 {
        D::insert(&db, &crate::crud::auto_tests::blank())
            .await
            .unwrap();
    }

    let live = D::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .delete_where(&db)
        .await
        .unwrap();
    assert_eq!(live, 3, "软删除必须影响全部三行");
    assert!(
        D::find_all(&db).await.unwrap().is_empty(),
        "软删除后默认查询必须查不到"
    );
    let left = db
        .query("SELECT COUNT(*) AS n FROM docs WHERE deleted_at IS NOT NULL")
        .await
        .unwrap();
    assert_eq!(
        left[0].get("n").and_then(|v| v.as_i64()),
        Some(3),
        "行还在表里"
    );

    // 已经软删的行再软删一次 → 闸门 → 影响 0 行
    let again = D::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .delete_where(&db)
        .await
        .unwrap();
    assert_eq!(again, 0, "IS NULL 闸门必须挡住重复删除");

    // 硬删除：闸门关掉，已软删的行也必须被真的删掉
    let purged = D::query()
        .filter("name", Op::Eq, "x")
        .unwrap()
        .hard_delete_where(&db)
        .await
        .unwrap();
    assert_eq!(purged, 3);
    let rows = db.query("SELECT id FROM docs").await.unwrap();
    assert!(rows.is_empty(), "hard_delete_where 必须真的删掉行");
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `crud.rs` 的测试。**单独成文件**是因为它们会把 `crud.rs` 顶过项目硬规则
//! 「每个源文件 < 500 行」—— 实现与测试各自都在上限内（同 `query/sql_tests.rs`）。
//! 夹具（假 executor、测试实体）见 `fixtures.rs`。

use super::fixtures::*;
use crate::entity::*;
use crate::error::OrmError;
use ecat_data::{Dialect, Row};
use serde_json::json;

// ---- insert ----

/// 自增主键**不进列清单**，但要在 `RETURNING` 里出现（一步式的回填手段）。
/// 断言整串 SQL 而不是 `contains` —— 「语句出现在错误的上下文里」那种
/// 失效模式对 `contains` 是隐形的。
#[tokio::test]
async fn insert_skips_the_auto_increment_pk() {
    let spy = Spy::default();
    *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(1)])];
    U::insert(
        &spy,
        &U {
            id: 0,
            name: "alice".into(),
        },
    )
    .await
    .unwrap();
    let (sql, params) = last(&spy);
    assert_eq!(
        sql, r#"INSERT INTO "users" ("name") VALUES (?) RETURNING "id""#,
        "自增主键不该出现在列清单里"
    );
    assert_eq!(params, vec![json!("alice")], "id 不该被绑定");
}

#[tokio::test]
async fn insert_returns_the_new_id() {
    let spy = Spy::default();
    *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(7)])];
    let id = U::insert(
        &spy,
        &U {
            id: 0,
            name: "a".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(id, 7);
}

/// **MySQL 两步式必须包事务**（spec:585-589）。
///
/// `LAST_INSERT_ID()` 是连接作用域的：池下 `INSERT` 与
/// `SELECT LAST_INSERT_ID()` 若各自独立取连接，可能落到不同连接，
/// 取回**别的会话刚插入的 id** —— 静默错值，不报错。
///
/// 假 executor 只有一条「连接」，抓不到漂移；所以断言**调用序列与归属**：
/// 两条语句都必须归属事务（`tx`），且以 `commit` 收尾。
/// 直连发（`db`）就会在序列里露出来。
#[tokio::test]
async fn insert_two_step_runs_both_statements_inside_one_transaction() {
    let spy = Spy {
        dialect: Dialect::MySql,
        ..Default::default()
    };
    *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(7)])];

    let id = U::insert(
        &spy,
        &U {
            id: 0,
            name: "alice".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(id, 7);

    assert_eq!(
        spy.events(),
        vec!["transaction", "tx", "tx", "commit"],
        "INSERT 与 LAST_INSERT_ID 必须都归属事务，并以 commit 收尾"
    );
    let calls = spy.calls();
    assert_eq!(
        calls[0].0, "INSERT INTO `users` (`name`) VALUES (?)",
        "第一步是 INSERT"
    );
    assert_eq!(calls[0].1, vec![json!("alice")]);
    assert_eq!(
        calls[1].0, "SELECT LAST_INSERT_ID()",
        "第二步取回 id，且同在事务内"
    );
}

// ---- find_by_id ----

#[tokio::test]
async fn find_by_id_returns_none_when_absent() {
    let spy = Spy::default(); // rows 为空
    assert_eq!(U::find_by_id(&spy, 1).await.unwrap(), None);
}

#[tokio::test]
async fn find_by_id_maps_the_row() {
    let spy = Spy::default();
    *spy.rows.lock().unwrap() = vec![Row::new(
        vec!["id".into(), "name".into()],
        vec![json!(1), json!("alice")],
    )];
    assert_eq!(
        U::find_by_id(&spy, 1).await.unwrap(),
        Some(U {
            id: 1,
            name: "alice".into()
        })
    );
    let (sql, params) = last(&spy);
    assert_eq!(sql, r#"SELECT "id", "name" FROM "users" WHERE "id" = ?"#);
    assert_eq!(params, vec![json!(1)]);
}

// ---- find_all ----

/// 便利别名：等价于 `Self::query().fetch(db)`，走的是**同一条**查询路径
/// （不另写 SQL 生成），所以列清单、方言占位符、软删除闸门都一致。
#[tokio::test]
async fn find_all_selects_every_column_without_a_where() {
    let spy = Spy::default();
    *spy.rows.lock().unwrap() = vec![
        Row::new(
            vec!["id".into(), "name".into()],
            vec![json!(1), json!("alice")],
        ),
        Row::new(
            vec!["id".into(), "name".into()],
            vec![json!(2), json!("bob")],
        ),
    ];
    let all = U::find_all(&spy).await.unwrap();
    assert_eq!(
        all,
        vec![
            U {
                id: 1,
                name: "alice".into()
            },
            U {
                id: 2,
                name: "bob".into()
            }
        ]
    );
    let (sql, params) = last(&spy);
    assert_eq!(sql, r#"SELECT "id", "name" FROM "users""#);
    assert!(params.is_empty());
}

// ---- delete ----

#[tokio::test]
async fn delete_by_pk_uses_the_pk_column() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    let n = U::delete_by_id(&spy, 5).await.unwrap();
    assert_eq!(n, 1);
    let (sql, params) = last(&spy);
    assert_eq!(sql, r#"DELETE FROM "users" WHERE "id" = ?"#);
    assert_eq!(params, vec![json!(5)]);
}

/// 找不到时返回 `NotFound` 而不是静默成功 —— 否则调用方以为删掉了。
#[tokio::test]
async fn delete_by_id_reports_missing_row() {
    let spy = Spy {
        affected: 0,
        ..Default::default()
    };
    let e = U::delete_by_id(&spy, 5).await.unwrap_err();
    assert!(matches!(e, OrmError::NotFound), "got: {e:?}");
}

// ---- update ----

#[tokio::test]
async fn update_writes_only_non_pk_columns() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    U::update(
        &spy,
        &U {
            id: 3,
            name: "bob".into(),
        },
    )
    .await
    .unwrap();
    let (sql, params) = last(&spy);
    assert_eq!(sql, r#"UPDATE "users" SET "name" = ? WHERE "id" = ?"#);
    assert_eq!(
        params,
        vec![json!("bob"), json!(3)],
        "SET 参数在前、WHERE 参数在后"
    );
}

#[tokio::test]
async fn update_reports_missing_row() {
    let spy = Spy {
        affected: 0,
        ..Default::default()
    };
    let e = U::update(
        &spy,
        &U {
            id: 99,
            name: "x".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(e, OrmError::NotFound), "got: {e:?}");
}

// ---- save ----

/// `save` 对**自增**且主键为 0 的实体走 insert。
#[tokio::test]
async fn save_inserts_when_pk_is_unset() {
    let spy = Spy::default();
    *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(9)])];
    let id = U::save(
        &spy,
        &U {
            id: 0,
            name: "new".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(id, 9);
    assert!(last(&spy).0.starts_with("INSERT"), "got: {}", last(&spy).0);
}

#[tokio::test]
async fn save_updates_when_pk_is_set() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    U::save(
        &spy,
        &U {
            id: 4,
            name: "old".into(),
        },
    )
    .await
    .unwrap();
    assert!(last(&spy).0.starts_with("UPDATE"), "got: {}", last(&spy).0);
}

/// **非自增主键永远是「已设置」，哪怕它的值是 0。**
///
/// 判据是 `auto_increment && pk == 0`，不是 `pk == 0`。用后者，手工分配整数
/// 主键的实体（主键正好是 0）每次 `save` 都会变成 INSERT —— 主键冲突，
/// 或悄无声息地多出一行。
#[tokio::test]
async fn save_updates_a_manual_pk_even_when_it_is_zero() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    P::save(
        &spy,
        &P {
            id: 0,
            name: "zero".into(),
        },
    )
    .await
    .unwrap();
    let (sql, _) = last(&spy);
    assert!(
        sql.starts_with("UPDATE"),
        "手工主键 0 也必须走 UPDATE，而不是 INSERT: {sql}"
    );
}

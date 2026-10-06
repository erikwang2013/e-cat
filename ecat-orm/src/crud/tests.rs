// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `crud.rs` 的测试。**单独成文件**是因为它们会把 `crud.rs` 顶过项目硬规则
//! 「每个源文件 < 500 行」—— 实现与测试各自都在上限内（同 `query/sql_tests.rs`）。
//! 夹具（假 executor、测试实体）见 `fixtures.rs`。

use super::fixtures::*;
use crate::entity::*;
use crate::error::OrmError;
use ecat_data::{Dialect, RdbmsClient, Row, SqlExecutor};
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

/// **在调用方的事务里 insert**（spec §5.4：`User::insert(&tx, &user)`）。
///
/// 与上一条是同一能力的两条路径：客户端那条（`insert(&client, …)`）由
/// `execute_then_query` **自己开一个**事务；`Transaction` 上的覆写**复用
/// 调用方已有的**事务 —— 不再开第二个，也**不得替调用方 commit**。
///
/// `events` 先清空：开头那次 `transaction` 是**测试自己**开事务记下的，
/// 与本方法无关；清掉后序列里剩下的每一条都是 insert 自己造成的。
#[tokio::test]
async fn insert_through_a_transaction_reuses_it_instead_of_opening_another() {
    let spy = Spy {
        dialect: Dialect::MySql,
        ..Default::default()
    };
    *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(7)])];

    let tx = spy.transaction().await.unwrap();
    spy.events.lock().unwrap().clear();

    let id = U::insert(
        &tx,
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
        vec!["tx", "tx"],
        "两条语句都归属调用方的事务；不得再开事务，也不得替调用方 commit"
    );
    let calls = spy.calls();
    assert_eq!(calls[0].0, "INSERT INTO `users` (`name`) VALUES (?)");
    assert_eq!(calls[1].0, "SELECT LAST_INSERT_ID()");

    // commit 只能由调用方发起，且只此一次 —— 上面若提前提交，这里会看到两次。
    tx.commit().await.unwrap();
    assert_eq!(spy.events(), vec!["tx", "tx", "commit"]);
}

/// **spec §5.4 的事务示例本身**：`let tx = db.transaction().await?;`
/// 然后 `User::insert(&tx, &user).await?`，最后 commit。
///
/// 这条测试的存在理由就是**让那行代码被编译到**。只断言「`insert` 接受
/// `SqlExecutor`」是空验收 —— bound 改回 `RdbmsClient` 它照样绿。
/// 唯有真的把 `&tx` 传进去（`Transaction` **不**实现 `RdbmsClient`），
/// 才验得出「事务里能 insert」。
#[tokio::test]
async fn insert_inside_the_callers_transaction_lands_on_commit() {
    let db = mem_sqlite("insert_in_tx").await;
    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
        .await
        .unwrap();

    let tx = db.transaction().await.unwrap();
    let id = U::insert(
        &tx,
        &U {
            id: 0,
            name: "alice".into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // 提交后经**客户端**读回：值与事务里那条 INSERT 一致 ——
    // 语句确实落在事务所在的连接上，且真的写进去了。
    assert_eq!(
        U::find_by_id(&db, id).await.unwrap(),
        Some(U {
            id,
            name: "alice".into()
        })
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
///
/// `save` **不回吐主键**（Task 14 改的返回类型）：需要新生成的主键时用
/// [`Entity::insert`]。所以这里断言的是「INSERT 真的发出去了」，不是返回值。
#[tokio::test]
async fn save_inserts_when_pk_is_unset() {
    let spy = Spy::default();
    *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(9)])];
    U::save(
        &spy,
        &U {
            id: 0,
            name: "new".into(),
        },
    )
    .await
    .unwrap();
    let (sql, params) = last(&spy);
    assert_eq!(
        sql,
        r#"INSERT INTO "users" ("name") VALUES (?) RETURNING "id""#
    );
    assert_eq!(params, vec![json!("new")]);
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

/// **字符串主键（UUID）的 `save` 必须成功**（Task 13 实施者报回的写后错）。
///
/// 旧实现里 `save` 返回 `i64`，更新路径要把主键 `as_i64()` —— 对 UUID 必然失败，
/// 于是在 **UPDATE 已经成功之后**才报错：数据写进去了，调用方却拿到 `Err`。
/// `save` 的语义本就是「存进去」（insert 或 update 二选一），不回吐主键。
#[tokio::test]
async fn save_of_a_string_pk_entity_succeeds() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    let s = S {
        id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
        name: "uuid".into(),
    };
    S::save(&spy, &s)
        .await
        .expect("字符串主键的 save 不得报错（旧实现在 UPDATE 成功之后才报）");
    let (sql, params) = last(&spy);
    assert_eq!(sql, r#"UPDATE "uuid_rows" SET "name" = ? WHERE "id" = ?"#);
    assert_eq!(params, vec![json!("uuid"), json!(s.id)]);
}

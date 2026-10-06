// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 分块数学 + `insert_many` / `update_many` / `upsert`。
//! 删除路径见 `delete_tests.rs`（两者合起来会顶过「每个源文件 < 500 行」）。
//!
//! 夹具全部复用 `crud` 的：`U` / `P`（无自动行为）、`D`（四种自动行为全开）、
//! `Spy`（记录 `(sql, params)` 的假 executor）、`mem_sqlite`（真库）。

use super::*;
use crate::crud::auto_tests::{assert_filled_now, blank, stored};
use crate::crud::fixtures::*;
use ecat_data::{Dialect, SqlExecutor};
use serde_json::json;

// ---- 分块数学 ----

#[test]
fn chunk_size_divides_the_limit_by_columns_per_row() {
    assert_eq!(chunk_size(2100, 3), 700);
    assert_eq!(chunk_size(999, 2), 499);
}

/// 向下取整：3.33 行 → 3 行。**进位就超限**。
#[test]
fn chunk_size_rounds_down() {
    assert_eq!(chunk_size(10, 3), 3);
}

/// **每行参数数超过上限时至少为 1** —— 返回 0 会让分块循环一行都不做
/// （静默丢数据），或让 `chunks(0)` 直接 panic。列数是用户定义的，不是常量。
#[test]
fn chunk_size_never_returns_zero() {
    assert_eq!(chunk_size(2, 5), 1);
    assert_eq!(chunk_size(1, 100), 1);
}

#[test]
fn splitting_covers_every_row_exactly_once() {
    let chunks = split_chunks(10, 3);
    assert_eq!(chunks, vec![3, 3, 3, 1]);
    assert_eq!(chunks.iter().sum::<usize>(), 10);
}

#[test]
fn splitting_handles_an_exact_multiple() {
    assert_eq!(split_chunks(9, 3), vec![3, 3, 3]);
}

#[test]
fn splitting_nothing_yields_no_chunks() {
    assert!(split_chunks(0, 100).is_empty());
}

#[test]
fn chunk_boundaries_align_with_the_param_limit() {
    let chunks = split_chunks(2100, chunk_size(2100, 3));
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks.iter().sum::<usize>(), 2100);
}

// ---- insert_many ----

/// **分块 + 累加**：SQLite 上限 999，`U` 每行 1 个可插列 →
/// 1000 行切 `[999, 1]`，返回值是两块之和（2）而不是最后一块（1）。
#[tokio::test]
async fn insert_many_splits_over_the_param_limit_and_sums_affected_rows() {
    let s = spy(Dialect::Sqlite, 1);
    let rows: Vec<U> = (0..1000)
        .map(|i| U {
            id: 0,
            name: format!("u{i}"),
        })
        .collect();
    assert_eq!(insert_many(&s, &rows).await.unwrap(), 2);

    let calls = s.calls();
    assert_eq!(calls.len(), 2, "1000 行 @999 必须切成两块");
    assert_eq!(calls[0].1.len(), 999);
    assert_eq!(calls[1].1.len(), 1);
    // 每块是**一条**多行 INSERT（逐行发语句同样不超限，但那不是批量）。
    assert_eq!(calls[0].0.matches("(?)").count(), 999);
    assert_eq!(calls[1].0.matches("(?)").count(), 1);
}

/// 上限之内只发**一条**语句，参数按行序展开。
#[tokio::test]
async fn insert_many_below_the_limit_is_a_single_statement() {
    let s = spy(Dialect::Sqlite, 3);
    let rows = vec![
        U {
            id: 0,
            name: "a".into(),
        },
        U {
            id: 0,
            name: "b".into(),
        },
    ];
    assert_eq!(insert_many(&s, &rows).await.unwrap(), 3);
    let calls = s.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0,
        r#"INSERT INTO "users" ("name") VALUES (?), (?)"#
    );
    assert_eq!(calls[0].1, vec![json!("a"), json!("b")]);
}

/// 空输入**不发语句** —— 生成 `VALUES ` 是语法错误，而「什么都不做」是合法语义。
#[tokio::test]
async fn insert_many_of_nothing_issues_no_statement_and_returns_zero() {
    let s = spy(Dialect::Sqlite, 7);
    let rows: Vec<U> = vec![];
    assert_eq!(insert_many(&s, &rows).await.unwrap(), 0);
    assert!(s.calls().is_empty(), "空输入不得发语句: {:?}", s.calls());
}

/// **每一行都各自跑一遍自动时间戳** —— 复用 `crud::insert_parts` 的意义所在。
/// 只对第一行调用的实现在这里红。
#[tokio::test]
async fn insert_many_fills_auto_timestamps_on_every_row() {
    let s = spy(Dialect::Sqlite, 2);
    let rows = vec![blank(), blank()];
    assert_eq!(insert_many(&s, &rows).await.unwrap(), 2);
    let calls = s.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0,
        r#"INSERT INTO "docs" ("name", "created_at", "updated_at", "deleted_at", "version") VALUES (?, ?, ?, ?, ?), (?, ?, ?, ?, ?)"#
    );
    assert_filled_now(&calls[0].1[1]);
    assert_filled_now(&calls[0].1[6]);
    assert_eq!(
        calls[0].1[4],
        json!(0),
        "第一行的 version 原样写入，insert 不改它"
    );
    assert_eq!(calls[0].1[9], json!(0), "第二行同样");
}

/// **65535 的「多一行」边界**：`P` 每行 2 个参数（非自增主键也要写），
/// PG / MySQL 的上限 65535 → 每块 32767 行，32768 行必须切两块。
///
/// 既有的 999 / 2100 覆盖不到这一档：把 65535 写成 `u16::MAX + 1` 之类的
/// 实现在那两条上照样绿。
#[tokio::test]
async fn insert_many_splits_one_row_over_the_65535_limit() {
    for d in [Dialect::Postgres, Dialect::MySql] {
        let s = spy(d, 1);
        let rows: Vec<P> = (0..32768)
            .map(|i| P {
                id: i,
                name: "n".into(),
            })
            .collect();
        assert_eq!(insert_many(&s, &rows).await.unwrap(), 2, "{d:?}");
        let calls = s.calls();
        assert_eq!(calls.len(), 2, "{d:?}: 32768 行 @2 参数/行 必须切两块");
        assert_eq!(calls[0].1.len(), 65534, "{d:?}");
        assert_eq!(calls[1].1.len(), 2, "{d:?}");
    }
}

/// **最紧的上限（2100）真的被用上了**：拿 65535 分块的实现会在 MSSQL 上
/// 发出一条 2100+ 参数的语句，真库直接拒收。
#[tokio::test]
async fn insert_many_respects_the_mssql_2100_limit() {
    let s = spy(Dialect::Mssql, 1);
    let rows: Vec<P> = (0..1051)
        .map(|i| P {
            id: i,
            name: "n".into(),
        })
        .collect();
    assert_eq!(insert_many(&s, &rows).await.unwrap(), 2);
    let calls = s.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1.len(), 2100, "一块的参数数正好是上限");
    assert_eq!(calls[1].1.len(), 2);
    // 占位符编号跨行连续 —— 每行从 @P1 重来是静默错值。
    assert!(
        calls[0]
            .0
            .starts_with("INSERT INTO [manual] ([id], [name]) VALUES (@P1, @P2), (@P3, @P4)"),
        "got: {}",
        calls[0].0
    );
}

// ---- update_many ----

/// SQLite 上限 999，`U` 每行 2 个参数（SET 的 name + 定位的 pk）→ 每块 499 行，
/// 500 行必须切两块，受影响行数累加。
#[tokio::test]
async fn update_many_splits_over_the_param_limit_and_sums_affected_rows() {
    let s = spy(Dialect::Sqlite, 1);
    let rows: Vec<U> = (0..500)
        .map(|i| U {
            id: i,
            name: format!("u{i}"),
        })
        .collect();
    assert_eq!(update_many(&s, &rows).await.unwrap(), 2);
    let calls = s.calls();
    assert_eq!(calls.len(), 2, "500 行 @499 必须切成两块");
    assert_eq!(calls[0].1.len(), 998);
    assert_eq!(calls[1].1.len(), 2);
    assert_eq!(calls[0].0.matches("(?, ?)").count(), 499, "一条多行 UPDATE");
    assert!(
        calls[0]
            .0
            .ends_with(r#"AS v WHERE "users"."id" = v."column1""#),
        "got: {}",
        calls[0].0
    );
}

#[tokio::test]
async fn update_many_of_nothing_issues_no_statement_and_returns_zero() {
    let s = spy(Dialect::Sqlite, 5);
    let rows: Vec<U> = vec![];
    assert_eq!(update_many(&s, &rows).await.unwrap(), 0);
    assert!(s.calls().is_empty(), "空输入不得发语句: {:?}", s.calls());
}

/// 逐行参数顺序是「主键、SET 的值、WHERE 的旧版本号」—— 顺序错就是
/// 把 A 行的值静默写进 B 行。同时钉住乐观锁的「SET 写新值、WHERE 比对旧值」。
#[tokio::test]
async fn update_many_orders_params_per_row_and_bumps_the_version() {
    let s = spy(Dialect::Sqlite, 2);
    let mut second = stored();
    second.id = 2;
    second.version = 7;
    assert_eq!(update_many(&s, &[stored(), second]).await.unwrap(), 2);

    let calls = s.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0,
        r#"UPDATE "docs" SET "name" = v."column2", "created_at" = v."column3", "updated_at" = v."column4", "deleted_at" = v."column5", "version" = v."column6" FROM (VALUES (?, ?, ?, ?, ?, ?, ?), (?, ?, ?, ?, ?, ?, ?)) AS v WHERE "docs"."id" = v."column1" AND "docs"."version" = v."column7""#
    );
    let p = &calls[0].1;
    assert_eq!(p[0], json!(1), "每行第一个是主键");
    assert_eq!(p[2], serde_json::Value::Null, "created_at 原样带上");
    assert_filled_now(&p[3]);
    assert_eq!(p[5], json!(6), "SET 写的是旧值 + 1");
    assert_eq!(p[6], json!(5), "WHERE 比对的是旧值");
    assert_eq!(p[7], json!(2), "第二行的参数从头开始");
    assert_eq!(p[12], json!(8));
    assert_eq!(p[13], json!(7));
    assert_filled_now(&p[10]);
}

// ---- upsert ----

/// upsert 是**单行**版（主键只有一个值）；批量插入用 `insert_many`。
#[tokio::test]
async fn upsert_sends_the_dialect_statement_and_returns_affected_rows() {
    let s = spy(Dialect::Sqlite, 1);
    let p = P {
        id: 7,
        name: "n".into(),
    };
    assert_eq!(upsert(&s, &p).await.unwrap(), 1);
    let (sql, params) = last(&s);
    assert_eq!(
        sql,
        r#"INSERT INTO "manual" ("id", "name") VALUES (?, ?) ON CONFLICT ("id") DO UPDATE SET "name" = excluded."name""#
    );
    assert_eq!(params, vec![json!(7), json!("n")]);
}

// ---- 真库上的端到端 ----

/// `UPDATE … FROM (VALUES …)` 这条形状**假 executor 证明不了真库会收**
/// （Task 12 实测：真库必拒的 SQL，29 条测试里 28 条照绿）。
#[tokio::test]
async fn batch_round_trips_on_a_real_database() {
    let db = mem_sqlite("batch").await;
    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
        .await
        .unwrap();

    let rows: Vec<U> = (0..3)
        .map(|i| U {
            id: 0,
            name: format!("before{i}"),
        })
        .collect();
    assert_eq!(insert_many(&db, &rows).await.unwrap(), 3, "三行必须都进库");
    let got = U::find_all(&db).await.unwrap();
    assert_eq!(got.len(), 3);

    let mut renamed = got.clone();
    for u in &mut renamed {
        u.name = format!("after{}", u.id);
    }
    assert_eq!(update_many(&db, &renamed).await.unwrap(), 3);

    let mut names: Vec<String> = U::find_all(&db)
        .await
        .unwrap()
        .iter()
        .map(|u| u.name.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["after1", "after2", "after3"],
        "多行 UPDATE 必须真的落库"
    );

    // upsert：同一主键第二次必须**更新**，不是插入第二行。
    db.execute("CREATE TABLE manual (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    let first = P {
        id: 1,
        name: "first".into(),
    };
    let second = P {
        id: 1,
        name: "second".into(),
    };
    assert_eq!(upsert(&db, &first).await.unwrap(), 1);
    assert_eq!(upsert(&db, &second).await.unwrap(), 1);
    let n = db.query("SELECT COUNT(*) AS n FROM manual").await.unwrap();
    assert_eq!(
        n[0].get("n").and_then(|v| v.as_i64()),
        Some(1),
        "不得插入第二行"
    );
    let name = db.query("SELECT name FROM manual").await.unwrap();
    assert_eq!(name[0].get("name").and_then(|v| v.as_str()), Some("second"));
}

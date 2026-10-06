// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `relation.rs` 的测试：分组纯函数、预加载执行、以及 **N+1 的 SQL 计数断言**。
//!
//! 「真的没发 N+1」单测证明不了 —— 只能数 SQL 条数，夹具见 `relation::fixtures`。

use super::fixtures::*;
use super::*;
use ecat_data::Dialect;
use serde_json::json;

// ---- 纯函数 ----

#[test]
fn grouping_buckets_rows_by_key() {
    let rows = vec![
        (Value::from(1), "a".to_string()),
        (Value::from(1), "b".to_string()),
        (Value::from(2), "c".to_string()),
    ];
    let g = group_by_key(rows);
    assert_eq!(g.get(&Value::from(1)).map(Vec::len), Some(2));
    assert_eq!(g.get(&Value::from(2)).map(Vec::len), Some(1));
    assert_eq!(g.get(&Value::from(3)), None);
}

/// 主体列表里的重复 local_key 只该查一次 —— 否则 100 行同属一个用户时
/// 会往 IN 列表里塞 100 个相同的 id。
#[test]
fn distinct_keys_are_deduplicated() {
    let keys = distinct_keys(&[Value::from(1), Value::from(1), Value::from(2)]);
    assert_eq!(keys.len(), 2);
}

/// NULL 的 local_key 不参与 IN 查询：`IN (NULL)` 恒为 UNKNOWN，
/// 查不出任何行，只是白跑一趟。
#[test]
fn null_keys_are_skipped() {
    let keys = distinct_keys(&[Value::from(1), Value::Null]);
    assert_eq!(keys, vec![Value::from(1)]);
}

#[test]
fn empty_subject_list_issues_no_query() {
    assert!(distinct_keys(&[]).is_empty());
}

/// **HasOne / BelongsTo 只取第一行。**
#[test]
fn single_valued_relations_take_only_the_first_row() {
    assert!(is_single_valued(RelationKind::HasOne));
    assert!(is_single_valued(RelationKind::BelongsTo));
    assert!(!is_single_valued(RelationKind::HasMany));
}

/// **三个方向各自的「两边是哪一列」必须分清** —— 写反了会查出一堆无关行
/// （不报错，只是结果错）。
#[test]
fn join_sides_are_derived_per_kind() {
    // HasMany: 本表 local_key ← 目标表 foreign_key
    let (mine, theirs) = join_sides(RelationKind::HasMany);
    assert_eq!(mine, Side::Subject);
    assert_eq!(theirs, Side::Target);
    // HasOne 同 HasMany
    let (mine, theirs) = join_sides(RelationKind::HasOne);
    assert_eq!(mine, Side::Subject);
    assert_eq!(theirs, Side::Target);
    // BelongsTo: 反过来 —— 本表 foreign_key ← 目标表 local_key
    let (mine, theirs) = join_sides(RelationKind::BelongsTo);
    assert_eq!(mine, Side::Target);
    assert_eq!(theirs, Side::Subject);
}

// ---- 预加载执行 ----

/// **本任务的核心断言**：3 个用户 + 他们的 posts，只发 2 条 SELECT。
///
/// N+1 的失败形态是 4 条（1 条主体 + 每用户 1 条），**只在条数上可分辨** ——
/// 回填结果对不对，两种写法都一样。
#[tokio::test]
async fn preloading_issues_one_in_query_for_all_subjects() {
    let db = Queue::new(Dialect::Sqlite);
    db.push(vec![
        rb_user_row(1, "alice", "red"),
        rb_user_row(2, "bob", "blue"),
        rb_user_row(3, "carol", "red"),
    ]);
    db.push(vec![
        rb_post_row(10, 1, "a"),
        rb_post_row(11, 1, "b"),
        rb_post_row(12, 2, "c"),
        // user 3 没有 post —— 没有关联行的主体必须被清成空，不是旧数据
    ]);

    let users = RbUser::query()
        .with(&[RbUserRelation::Posts])
        .fetch(&db)
        .await
        .expect("fetch must succeed");

    let calls = db.calls();
    assert_eq!(calls.len(), 2, "1 条主体 + 1 条 IN；逐主体查会变成 4 条");
    assert_eq!(
        calls[0].0,
        r#"SELECT "id", "name", "tag_code" FROM "rb_users""#
    );
    assert_eq!(
        calls[1].0,
        r#"SELECT * FROM "rb_posts" WHERE "user_id" IN (?, ?, ?)"#
    );
    assert_eq!(calls[1].1, vec![json!(1), json!(2), json!(3)]);

    assert_eq!(users[0].posts.len(), 2);
    assert_eq!(users[0].posts[1].title, "b");
    assert_eq!(users[1].posts.len(), 1);
    assert!(users[2].posts.is_empty(), "没有关联行的主体必须是空");
}

/// 重复的主体键只进 IN 列表一次 —— 并且这里走的是**非主键** local_key
/// （`local_key = "name"`，值从 `to_values()` 里取）。
///
/// 回填那半边同样钉住：同一个键的**两个**主体都该拿到那份关联行
/// （`remove` 只有第一个拿得到，其余静默变空）。
#[tokio::test]
async fn duplicate_subject_keys_are_queried_once_and_shared() {
    let db = Queue::new(Dialect::Sqlite);
    db.push(vec![
        rb_user_row(1, "alice", "red"),
        rb_user_row(2, "bob", "blue"),
        rb_user_row(3, "alice", "red"),
    ]);
    db.push(vec![rb_comment_row(100, "alice", "hi")]);

    let users = RbUser::query()
        .with(&[RbUserRelation::Comments])
        .fetch(&db)
        .await
        .expect("fetch must succeed");

    let calls = db.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].0,
        r#"SELECT * FROM "rb_comments" WHERE "post_id" IN (?, ?)"#
    );
    assert_eq!(
        calls[1].1,
        vec![json!("alice"), json!("bob")],
        "重复的 name 只查一次"
    );

    // 两个 alice 都拿到那条 comment；bob 没有
    assert_eq!(users[0].comments.len(), 1);
    assert!(users[1].comments.is_empty());
    assert_eq!(users[2].comments.len(), 1, "同键的第二个主体也要拿到");
}

/// **BelongsTo 的两列方向与 HasMany 相反**：主体取自己的 `tag_code`，
/// 去目标表的 `code` 上匹配。写反了 SQL 不报错，只是查出一堆无关行 ——
/// 所以这里连 SQL 与参数一起钉。
#[tokio::test]
async fn belongs_to_matches_the_target_key_not_the_subject_pk() {
    let db = Queue::new(Dialect::Sqlite);
    db.push(vec![
        rb_user_row(1, "alice", "red"),
        rb_user_row(2, "bob", "blue"),
    ]);
    db.push(vec![rb_tag_row("red", "Red"), rb_tag_row("blue", "Blue")]);

    let users = RbUser::query()
        .with(&[RbUserRelation::Tag])
        .fetch(&db)
        .await
        .expect("fetch must succeed");

    let calls = db.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].0,
        r#"SELECT * FROM "rb_tags" WHERE "code" IN (?, ?)"#
    );
    assert_eq!(
        calls[1].1,
        vec![json!("red"), json!("blue")],
        "取的是主体的 tag_code，不是主键 id"
    );
    assert_eq!(users[0].tag.as_ref().map(|t| t.label.as_str()), Some("Red"));
    assert!(users[1].tag.is_some());
}

/// HasOne 端到端：库里同键有两行时只留第一行。
#[tokio::test]
async fn has_one_keeps_the_first_row() {
    let db = Queue::new(Dialect::Sqlite);
    db.push(vec![rb_user_row(1, "alice", "red")]);
    db.push(vec![
        rb_profile_row(20, 1, "first"),
        rb_profile_row(21, 1, "second"),
    ]);

    let users = RbUser::query()
        .with(&[RbUserRelation::Profile])
        .fetch(&db)
        .await
        .expect("fetch must succeed");

    assert_eq!(users[0].profile.as_ref().map(|p| p.id), Some(20));
}

/// 未声明的关联名必须在发关联查询前就报错。
///
/// 注意：这也顺带证明 `with()` 真的把关联名传到了加载器 —— 若 `with` 丢掉了
/// 名字，这个查询会安静地成功（一条关联查询都不发）。
#[tokio::test]
async fn unknown_relation_names_are_rejected() {
    /// 手写选择器：派生枚举只会产出真实存在的关联名，这个用例要的是「不存在的」。
    #[derive(Clone, Copy)]
    struct Bogus;

    impl RelationSelector for Bogus {
        fn name(self) -> &'static str {
            "nope"
        }
    }

    let db = Queue::new(Dialect::Sqlite);
    db.push(vec![rb_user_row(1, "alice", "red")]);

    let err = RbUser::query()
        .with(&[Bogus])
        .fetch(&db)
        .await
        .expect_err("unknown relation must fail");

    assert!(matches!(err, OrmError::UnknownColumn(_)), "got: {err:?}");
    assert_eq!(db.calls().len(), 1, "只发了主体查询");
}

// ---- 探针：只有手写实体才观察得到的行为 ----

#[tokio::test]
async fn single_valued_relations_get_at_most_one_row() {
    let db = Queue::new(Dialect::Sqlite);
    db.push(vec![probe_row("k"), probe_row("k"), probe_row("k")]);

    let mut probes = vec![probe("k")];
    load_relation::<Probe, _>(&db, &mut probes, "one")
        .await
        .expect("load must succeed");

    assert_eq!(
        probes[0].writes,
        vec![("one".to_string(), 1)],
        "三行只该交一行给单值 setter"
    );
}

/// **即使一条关联行都没有，也必须调用 `set_relation`** —— 否则字段里
/// 残留的是上一次加载的旧数据（静默给过时数据）。主体键全是 NULL 时
/// 一条语句都不发，写回却一条都不能少。
#[tokio::test]
async fn subjects_without_a_key_still_get_written_back() {
    let db = Queue::new(Dialect::Sqlite);

    let mut probes = vec![Probe::default(), Probe::default()];
    load_relation::<Probe, _>(&db, &mut probes, "one")
        .await
        .expect("load must succeed");

    assert!(
        db.calls().is_empty(),
        "没有可查的键就不发语句（IN () 是语法错误）"
    );
    let expected = vec![("one".to_string(), 0)];
    assert_eq!(probes[0].writes, expected);
    assert_eq!(probes[1].writes, expected);
}

/// 键多到超过方言的单语句参数上限时必须**分块**，不能一口气全塞进一条 IN。
#[tokio::test]
async fn in_lists_are_chunked_by_the_dialect_limit() {
    let db = Queue::new(Dialect::Sqlite); // 上限 999，每键 1 个参数

    let mut probes: Vec<Probe> = (0..1000).map(|i| probe(&i.to_string())).collect();
    load_relation::<Probe, _>(&db, &mut probes, "one")
        .await
        .expect("load must succeed");

    let calls = db.calls();
    assert_eq!(calls.len(), 2, "999 + 1：不分块就是一条 1000 个参数的语句");
    assert_eq!(calls[0].1.len(), 999);
    assert_eq!(
        calls[1].0,
        r#"SELECT * FROM "probe_targets" WHERE "owner" IN (?)"#
    );
    assert_eq!(calls[1].1.len(), 1);
}

/// 目标结果集里缺关联列时报错，而不是把「列不存在」当成「这个主体没有关联行」
/// 静默放过 —— 那正是 foreign_key / local_key 写反后的表现。
#[tokio::test]
async fn a_result_set_without_the_target_column_is_an_error() {
    let db = Queue::new(Dialect::Sqlite);
    db.push(vec![row(&["id"], vec![json!(1)])]);

    let mut probes = vec![probe("k")];
    let err = load_relation::<Probe, _>(&db, &mut probes, "one")
        .await
        .expect_err("missing target column must fail");

    assert!(matches!(err, OrmError::UnknownColumn(_)), "got: {err:?}");
    assert!(probes[0].writes.is_empty(), "报错时不该写回");
}

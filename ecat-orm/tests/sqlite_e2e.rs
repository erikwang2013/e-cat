// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! SQLite 全链路集成测试（spec §12 第 4 条）：
//!
//! ```text
//! 实体定义 → 迁移建表 → CRUD → 关联预加载 → 分页 → 事务 → 软删除 → 乐观锁冲突
//! ```
//!
//! **这是本批唯一跑真数据库的链路测试。** `src/` 里的单测多是字符串断言与假
//! executor —— 它们证明「生成了我期望的 SQL」，不证明那些 SQL 在任何数据库上
//! 能跑通。批次 1/2 的真库用例各抓到一个 Critical（`UNSIGNED` 主键、`smallint`
//! 静默错值），差别就在这。
//!
//! 夹具（实体、临时库、计数执行器）在 `common/mod.rs`；本文件只放断言。

mod common;

use common::*;
use ecat_data::{RdbmsClient, SqlExecutor};
use ecat_orm::query::Order;
use ecat_orm::{Entity, OrmError};
use time::UtcOffset;

// ---- ① 迁移建表 → CRUD ----

#[tokio::test]
async fn migrations_build_the_schema_and_crud_round_trips() {
    let (db, path) = empty_db("crud").await;

    // 迁移账本：跑之前全部 pending，跑之后全部 applied（版本表也真的落库了）。
    let m = migrations(&db);
    let st = m.status().await.unwrap();
    assert_eq!(st.pending, vec![1, 2], "全新库上两条迁移都应待应用");
    assert!(st.applied.is_empty());
    m.run().await.unwrap();
    let st = m.status().await.unwrap();
    assert_eq!(st.applied, vec![1, 2], "重跑 run() 后应记为已应用");
    assert!(st.pending.is_empty());
    // 幂等：再跑一次不报错、不重复执行（建表语句本身是 IF NOT EXISTS，
    // 但版本表才是「不重复执行」的依据）。
    m.run().await.unwrap();

    // CREATE：自增主键真的由数据库回填（SQLite 走 RETURNING 一步式）。
    let id = User::insert(&db, &user("alice")).await.unwrap();
    assert!(id > 0, "自增主键必须由数据库生成，got: {id}");

    // READ
    let got = User::find_by_id(&db, id)
        .await
        .unwrap()
        .expect("刚插入的行");
    assert_eq!(got.name, "alice");
    assert!(got.created_at.is_some(), "insert 必须填 created_at");
    assert!(got.updated_at.is_some(), "insert 必须填 updated_at");
    assert_eq!(got.posts.len(), 0, "未预加载的关联必须是空，不是过时数据");
    assert_eq!(User::find_all(&db).await.unwrap().len(), 1);
    assert!(User::find_by_id(&db, 9999).await.unwrap().is_none());

    // UPDATE：整行写回，version 由乐观锁 +1。
    let mut edit = got.clone();
    edit.name = "alice-v2".into();
    edit.email = Some("alice@example.com".into());
    User::update(&db, &edit).await.unwrap();
    let after = User::find_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(after.name, "alice-v2");
    assert_eq!(after.email.as_deref(), Some("alice@example.com"));
    assert_eq!(after.version, 2, "UPDATE 必须把 version 写成旧值 + 1");
    assert_eq!(after.created_at, got.created_at, "update 不得动 created_at");

    // 查询构建器（白名单 + ORDER BY + LIMIT）在真库上执行得通。
    User::insert(&db, &user("bob")).await.unwrap();
    let two = User::query()
        .filter("name", ecat_orm::query::Op::Like, "alice%")
        .unwrap()
        .order_by("id", Order::Asc)
        .unwrap()
        .fetch(&db)
        .await
        .unwrap();
    assert_eq!(two.len(), 1);
    assert_eq!(two[0].name, "alice-v2");

    // DELETE（软删除实体走 UPDATE，见 ④）
    User::delete_by_id(&db, id).await.unwrap();
    assert!(User::find_by_id(&db, id).await.unwrap().is_none());

    // 迁移可回滚：反向 SQL 真的能执行，版本表那一行也真的删掉。
    migrations(&db).down(2).await.unwrap();
    let st = migrations(&db).status().await.unwrap();
    assert_eq!(st.pending, vec![2], "回滚后 002 回到待应用");

    cleanup(&path);
}

// ---- ② 关联预加载：N+1 杜绝 ----

/// 3 个用户 + 他们的 posts，**总计只发 2 条 SELECT**（1 条主体 + 1 条 IN）。
///
/// 逐主体查的写法会发出 4 条 —— 这条断言把两者区分开。内容断言（每条 post
/// 归对了人）用来防「只发了 2 条但把关联装错了」这种更隐蔽的错。
#[tokio::test]
async fn relation_preload_issues_two_selects_for_three_subjects() {
    let (db, path) = migrated_db("n_plus_1").await;

    let alice = User::insert(&db, &user("alice")).await.unwrap();
    let bob = User::insert(&db, &user("bob")).await.unwrap();
    let carol = User::insert(&db, &user("carol")).await.unwrap();
    Post::insert_many(
        &db,
        &[
            post(alice, "a1"),
            post(alice, "a2"),
            post(bob, "b1"),
            post(carol, "c1"),
        ],
    )
    .await
    .unwrap();

    let counting = Counting::new(&db);
    let users = User::query()
        .with(&[UserRelation::Posts])
        .order_by("id", Order::Asc)
        .unwrap()
        .fetch(&counting)
        .await
        .unwrap();

    let stmts = counting.statements();
    assert_eq!(
        stmts.len(),
        2,
        "3 个主体 + 关联只该发 2 条 SELECT（1 主体 + 1 IN），逐主体查会是 4 条；实发：{stmts:#?}"
    );
    assert!(
        stmts[0].contains(r#"FROM "e2e_users""#),
        "第一条是主体查询，got: {}",
        stmts[0]
    );
    assert!(
        stmts[1].contains(" IN (") && stmts[1].contains(r#"FROM "e2e_posts""#),
        "第二条是关联的 IN 查询（含 3 个去重键），got: {}",
        stmts[1]
    );

    assert_eq!(users.len(), 3);
    let titles = |u: &User| u.posts.iter().map(|p| p.title.clone()).collect::<Vec<_>>();
    assert_eq!(titles(&users[0]), vec!["a1", "a2"]);
    assert_eq!(titles(&users[1]), vec!["b1"]);
    assert_eq!(
        titles(&users[2]),
        vec!["c1"],
        "carol 的行必须归到 carol 名下"
    );

    // 对照：`with()` 之外不发关联查询 —— 少这条，把预加载做成「无条件全表查」
    // 也能让上面绿。
    let bare = Counting::new(&db);
    User::query().fetch(&bare).await.unwrap();
    assert_eq!(bare.statements().len(), 1, "不声明 with() 就只该有主体查询");

    cleanup(&path);
}

// ---- ③ 分页 ----

#[tokio::test]
async fn pagination_counts_and_slices_on_a_real_database() {
    let (db, path) = migrated_db("page").await;

    User::insert_many(
        &db,
        &[user("u0"), user("u1"), user("u2"), user("u3"), user("u4")],
    )
    .await
    .unwrap();

    let first = User::query()
        .order_by("id", Order::Asc)
        .unwrap()
        .paginate(&db, 1, 2)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(first.total, Some(5), "COUNT 复用同一个 WHERE，且不带 LIMIT");
    assert_eq!(
        first.total_pages(),
        Some(3),
        "5 条 / 每页 2 → 3 页（向上取整）"
    );
    assert!(first.has_next());

    let last = User::query()
        .order_by("id", Order::Asc)
        .unwrap()
        .paginate(&db, 3, 2)
        .await
        .unwrap();
    assert_eq!(last.items.len(), 1, "5 = 2 + 2 + 1，最后一页只剩一条");
    assert!(!last.has_next());

    // 越界页：给空页而不是报错，且总数仍如实报出。
    let over = User::query()
        .order_by("id", Order::Asc)
        .unwrap()
        .paginate(&db, 9, 2)
        .await
        .unwrap();
    assert!(over.items.is_empty());
    assert_eq!(over.total, Some(5));

    // 不数总数的那条路径：省掉 COUNT，就如实说「不知道」。
    let cheap = User::query()
        .order_by("id", Order::Asc)
        .unwrap()
        .paginate_without_count(&db, 1, 2)
        .await
        .unwrap();
    assert_eq!(cheap.items.len(), 2);
    assert_eq!(cheap.total, None);
    assert_eq!(cheap.total_pages(), None);
    assert!(!cheap.has_next(), "没总数就不说有下一页");

    cleanup(&path);
}

// ---- ④ 事务 ----

#[tokio::test]
async fn transactions_commit_and_roll_back_on_a_real_database() {
    let (db, path) = migrated_db("tx").await;

    // spec §5.4：`User::insert(&tx, &user)` —— 事务里插入，主键照样回填。
    let tx = db.transaction().await.unwrap();
    let id = User::insert(&tx, &user("in-tx")).await.unwrap();
    assert!(User::find_by_id(&tx, id).await.unwrap().is_some());
    tx.rollback().await.unwrap();
    assert!(
        User::find_by_id(&db, id).await.unwrap().is_none(),
        "回滚后该行不得可见（SQLite 一步式 INSERT 也必须在事务内）"
    );

    let tx = db.transaction().await.unwrap();
    let kept = User::insert(&tx, &user("committed")).await.unwrap();
    User::update(
        &tx,
        &User {
            name: "committed-v2".into(),
            ..User::find_by_id(&tx, kept).await.unwrap().unwrap()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let got = User::find_by_id(&db, kept)
        .await
        .unwrap()
        .expect("提交后必须落库");
    assert_eq!(got.name, "committed-v2", "事务内的 UPDATE 也应一起提交");
    assert_eq!(got.version, 2);

    cleanup(&path);
}

// ---- ⑤ 软删除 ----

#[tokio::test]
async fn soft_delete_hides_rows_but_keeps_them() {
    let (db, path) = migrated_db("soft_delete").await;

    let id = User::insert(&db, &user("gone")).await.unwrap();
    User::insert(&db, &user("stays")).await.unwrap();

    assert_eq!(
        User::delete_by_id(&db, id).await.unwrap(),
        1,
        "软删除影响 1 行"
    );
    assert!(User::find_by_id(&db, id).await.unwrap().is_none());
    assert_eq!(
        User::find_all(&db).await.unwrap().len(),
        1,
        "默认查询看不见"
    );

    // 行还在表里，且 `deleted_at` 真的被写上（不是发了 DELETE 却报成功）。
    let rows = db
        .query(&format!("SELECT deleted_at FROM e2e_users WHERE id = {id}"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "软删除是 UPDATE —— 行必须还在");
    assert!(
        rows[0].get("deleted_at").and_then(|v| v.as_str()).is_some(),
        "deleted_at 必须被置成时刻"
    );

    // `with_trashed()` 是读取开关：连软删的一起查回来。
    let all = User::query()
        .with_trashed()
        .order_by("id", Order::Asc)
        .unwrap()
        .fetch(&db)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].deleted_at.is_some() && all[1].deleted_at.is_none());

    // 重复删除 → NotFound，且不刷新删除时刻（`AND deleted_at IS NULL` 闸门）。
    let e = User::delete_by_id(&db, id).await.unwrap_err();
    assert!(matches!(e, OrmError::NotFound), "got: {e:?}");
    let again = db
        .query(&format!("SELECT deleted_at FROM e2e_users WHERE id = {id}"))
        .await
        .unwrap();
    assert_eq!(again[0].get("deleted_at"), rows[0].get("deleted_at"));

    // 需要真删的时候有硬删除的口子。
    User::hard_delete_by_id(&db, id).await.unwrap();
    assert_eq!(
        User::query().with_trashed().fetch(&db).await.unwrap().len(),
        1
    );

    cleanup(&path);
}

// ---- ⑥ 乐观锁冲突 ----

#[tokio::test]
async fn optimistic_lock_rejects_the_stale_writer() {
    let (db, path) = migrated_db("lock").await;

    let id = User::insert(&db, &user("contended")).await.unwrap();
    let first = User::find_by_id(&db, id).await.unwrap().unwrap();
    let second = first.clone();

    // 第二个写者先提交：version 1 → 2。
    User::update(
        &db,
        &User {
            name: "second".into(),
            ..second
        },
    )
    .await
    .unwrap();

    // 第一个写者拿着 version=1 的旧快照 → 真库影响 0 行 → 冲突（不是静默覆盖）。
    let e = User::update(
        &db,
        &User {
            name: "first".into(),
            ..first
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(e, OrmError::OptimisticLockConflict), "got: {e:?}");

    let now = User::find_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(now.name, "second", "被拒的写入不得覆盖别人的值");
    assert_eq!(now.version, 2);

    // 重新加载再写 → 成功。冲突是**可恢复**的，不是死路。
    User::update(
        &db,
        &User {
            name: "retry".into(),
            ..now
        },
    )
    .await
    .unwrap();
    assert_eq!(
        User::find_by_id(&db, id).await.unwrap().unwrap().name,
        "retry"
    );

    cleanup(&path);
}

// ---- ⑦ 时间列无 CAST 往返 ----

/// spec §12 第 5 条：时间列**不需要 CAST** 就能正确读写。
///
/// 写一个带 `+08:00` 偏移的时刻，读回来必须是**同一时刻**。
///
/// ⚠️ 「写入前归一化 UTC」这条规则**不能靠读回来的值来验证** —— SQLite 的读路径
/// （`ecat-data-sqlx/src/cell.rs` 的 `rfc3339()`）会把任何偏移量文本重新格式化回
/// UTC。实测：把 `time::to_rfc3339_utc` 的 `.to_offset(UTC)` 删掉，读回来**照样**
/// 是 `…Z`（这条断言曾经因此是空验收）。所以写入侧的归一化只在**绑定参数**上
/// 可见：走计数层抓 `INSERT` 实际绑了什么。
#[tokio::test]
async fn timestamps_round_trip_as_utc_without_any_cast() {
    let (db, path) = migrated_db("time").await;

    // 固定时刻（不是 `now()`）：断言要能逐字比对，也不受运行时刻影响。
    // 12:00+08:00 与 04:00Z 是同一时刻 —— 库里的文本不归一化就会是前者。
    let plus8 = time::macros::datetime!(2026-10-05 12:00:00 +8);
    assert_eq!(plus8.offset(), UtcOffset::from_hms(8, 0, 0).unwrap());

    let counting = Counting::new(&db);
    let id = User::insert(
        &counting,
        &User {
            created_at: Some(plus8),
            ..user("tz")
        },
    )
    .await
    .unwrap();

    // ① 写入侧：绑定的参数必须已是 RFC3339 **UTC**，不是 `+08:00` 原样。
    //    不做归一化的话，库里存的是 `+08:00` 文本，而 SQLite 上时间列就是文本 ——
    //    跨偏移的文本比较（ORDER BY / 范围过滤）会给出错误顺序。
    let (insert_sql, params) = counting
        .calls()
        .into_iter()
        .find(|(sql, _)| sql.starts_with("INSERT INTO \"e2e_users\""))
        .expect("必须记录到 INSERT");
    assert!(
        insert_sql.contains("\"created_at\""),
        "本条测的就是 created_at 的绑定值，got: {insert_sql}"
    );
    let bound: Vec<String> = params
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    assert!(
        bound.contains(&"2026-10-05T04:00:00Z".to_string()),
        "+08:00 的 12:00 必须绑成 UTC 的 04:00（同一时刻的 UTC 表示）；实绑：{bound:?}"
    );
    assert!(
        !bound.iter().any(|p| p.contains("+08:00")),
        "不得把带偏移量的原样文本喂给数据库；实绑：{bound:?}"
    );

    // ② 读回来：同一时刻，无需任何 CAST。
    let got = User::find_by_id(&db, id).await.unwrap().unwrap();
    let back = got.created_at.expect("created_at 不该是 NULL");
    assert_eq!(back, plus8, "往返必须保持同一时刻");
    assert_eq!(back.offset(), UtcOffset::UTC, "读回来必须是 UTC");
    assert_eq!(
        back.format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        "2026-10-05T04:00:00Z"
    );

    cleanup(&path);
}

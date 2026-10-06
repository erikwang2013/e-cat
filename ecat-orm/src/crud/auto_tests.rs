// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 自动行为（时间戳 / 软删除 / 乐观锁）的测试。**单独成文件**是为了守住
//! 「每个源文件 < 500 行」—— 追加进 `tests.rs` 会把它顶过上限。
//! 夹具（假 executor）见 `fixtures.rs`。
//!
//! 断言一律用**整串 `assert_eq!`**：软删除/乐观锁的断言若只查「SQL 里有没有
//! 某片段」，对「语句出现在错误的上下文里」恒真（Task 12 实测：把 ORDER BY 提到
//! WHERE 之前，真库会拒的 SQL，29 条测试里 28 条照绿）。

use super::fixtures::*;
use crate::entity::*;
use crate::error::OrmError;
use ecat_data::{Row, SqlExecutor};
use serde_json::json;
use time::{Duration, OffsetDateTime};

// ---- 夹具：四种自动行为全开 ----

static AUTO_COLS: [ColumnMeta; 6] = [
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
        name: "created_at",
        ty: ColType::Timestamp,
        nullable: true,
        pk: false,
        auto_increment: false,
    },
    ColumnMeta {
        name: "updated_at",
        ty: ColType::Timestamp,
        nullable: true,
        pk: false,
        auto_increment: false,
    },
    ColumnMeta {
        name: "deleted_at",
        ty: ColType::Timestamp,
        nullable: true,
        pk: false,
        auto_increment: false,
    },
    ColumnMeta {
        name: "version",
        ty: ColType::I64,
        nullable: false,
        pk: false,
        auto_increment: false,
    },
];
static AUTO_META: EntityMeta = EntityMeta {
    table: "docs",
    pk: "id",
    columns: &AUTO_COLS,
    relations: &[],
    flags: EntityFlags {
        created_at: Some("created_at"),
        updated_at: Some("updated_at"),
        soft_delete: Some("deleted_at"),
        version: Some("version"),
    },
};

#[derive(Debug, Clone)]
struct D {
    id: i64,
    name: String,
    created_at: Option<OffsetDateTime>,
    updated_at: Option<OffsetDateTime>,
    deleted_at: Option<OffsetDateTime>,
    version: i64,
}

impl Entity for D {
    const TABLE: &'static str = "docs";
    const PK: &'static str = "id";
    const META: &'static EntityMeta = &AUTO_META;
    fn from_row(r: &Row) -> Result<Self, OrmError> {
        Ok(D {
            id: crate::value::from_row_col::<i64>(r, "id")?,
            name: crate::value::from_row_col::<String>(r, "name")?,
            created_at: crate::value::from_row_col::<Option<OffsetDateTime>>(r, "created_at")?,
            updated_at: crate::value::from_row_col::<Option<OffsetDateTime>>(r, "updated_at")?,
            deleted_at: crate::value::from_row_col::<Option<OffsetDateTime>>(r, "deleted_at")?,
            version: crate::value::from_row_col::<i64>(r, "version")?,
        })
    }
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("id", json!(self.id)),
            ("name", json!(self.name)),
            (
                "created_at",
                crate::value::ColumnValue::to_json(&self.created_at),
            ),
            (
                "updated_at",
                crate::value::ColumnValue::to_json(&self.updated_at),
            ),
            (
                "deleted_at",
                crate::value::ColumnValue::to_json(&self.deleted_at),
            ),
            ("version", json!(self.version)),
        ]
    }
    fn pk_value(&self) -> serde_json::Value {
        json!(self.id)
    }
    fn set_relation(&mut self, _name: &str, _rows: Vec<Row>) -> Result<(), OrmError> {
        Ok(())
    }
}

/// 全空的实体：时间戳为 `None`、version 为 0。
fn blank() -> D {
    D {
        id: 0,
        name: "x".into(),
        created_at: None,
        updated_at: None,
        deleted_at: None,
        version: 0,
    }
}

/// 与 [`blank`] 比：主键已设置、version = 5（update/乐观锁用）。
fn stored() -> D {
    D {
        id: 1,
        version: 5,
        ..blank()
    }
}

const AUTO_FILLED: &str = "2020-01-01T00:00:00Z";

/// 一个**被填进去**的时间戳：必须是 RFC3339 字符串，且**接近此刻**。
///
/// 用性质断言而不是 `assert_eq!` 某个精确时刻：既能证明「填的是当前时间」，
/// 又不依赖具体时刻 —— 因此**不需要可注入的时钟**（见 `crud.rs` 的 `now()` 文档）。
fn assert_filled_now(v: &serde_json::Value) {
    let s = v
        .as_str()
        .unwrap_or_else(|| panic!("自动时间戳应是 RFC3339 字符串，得到 {v}"));
    let t = crate::time::from_rfc3339(s).unwrap_or_else(|e| panic!("{s} 不是 RFC3339: {e}"));
    let drift = (OffsetDateTime::now_utc() - t).abs();
    assert!(
        drift < Duration::minutes(1),
        "填进去的时间应接近此刻，偏差 {drift}：{s}"
    );
}

/// 建一个会返回自增主键的 spy。
fn inserting_spy() -> Spy {
    let spy = Spy::default();
    *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(1)])];
    spy
}

// ---- created_at / updated_at ----

#[tokio::test]
async fn insert_fills_created_at_when_none() {
    let spy = inserting_spy();
    D::insert(&spy, &blank()).await.unwrap();
    let (sql, params) = last(&spy);
    assert_eq!(
        sql,
        r#"INSERT INTO "docs" ("name", "created_at", "updated_at", "deleted_at", "version") VALUES (?, ?, ?, ?, ?) RETURNING "id""#
    );
    assert_eq!(params[0], json!("x"));
    assert_filled_now(&params[1]);
    assert_filled_now(&params[2]);
    assert_eq!(params[3], serde_json::Value::Null, "软删除列不该被填");
}

/// **显式值优先** —— created_at 是事实记录，数据导入时要保留原始创建时间。
#[tokio::test]
async fn insert_respects_an_explicit_created_at() {
    let spy = inserting_spy();
    let mut d = blank();
    d.created_at = Some(time::macros::datetime!(2020-01-01 00:00:00 UTC));
    D::insert(&spy, &d).await.unwrap();
    let (_, params) = last(&spy);
    assert_eq!(
        params[1],
        json!(AUTO_FILLED),
        "显式 created_at 必须原样保留"
    );
}

/// **与上面相反**：updated_at 无条件覆盖（spec:523「insert 与 update 时均填」）。
///
/// 这条的不对称是**故意的**：updated_at 是变更追踪，写入即代表「此刻被改过」，
/// 显式传旧值没有意义。不要为了「看着匀称」把它改成尊重显式值。
#[tokio::test]
async fn insert_overwrites_an_explicit_updated_at() {
    let spy = inserting_spy();
    let mut d = blank();
    d.updated_at = Some(time::macros::datetime!(2020-01-01 00:00:00 UTC));
    D::insert(&spy, &d).await.unwrap();
    let (_, params) = last(&spy);
    assert_ne!(params[2], json!(AUTO_FILLED), "updated_at 应被覆盖为此刻");
    assert_filled_now(&params[2]);
}

#[tokio::test]
async fn update_always_refreshes_updated_at() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    D::update(&spy, &stored()).await.unwrap();
    let (sql, params) = last(&spy);
    assert_eq!(
        sql,
        r#"UPDATE "docs" SET "name" = ?, "created_at" = ?, "updated_at" = ?, "deleted_at" = ?, "version" = ? WHERE "id" = ? AND "version" = ?"#
    );
    assert_eq!(
        params[1],
        serde_json::Value::Null,
        "update 不得动 created_at —— 它只在 insert 时填（spec:522）"
    );
    assert_ne!(params[2], json!(AUTO_FILLED), "updated_at 应被覆盖为此刻");
    assert_filled_now(&params[2]);
}

// ---- 乐观锁 ----

#[tokio::test]
async fn update_guards_on_version_and_bumps_it() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    D::update(&spy, &stored()).await.unwrap();
    let (sql, params) = last(&spy);
    // 逐位断言（时间戳那一格除外）：`contains(&json!(5))` 这类查法分不清
    // 「5 出现在 WHERE 的 version 上」还是「出现在别的列上」。
    assert_eq!(params[0], json!("x"));
    assert_eq!(params[3], serde_json::Value::Null);
    assert_eq!(params[4], json!(6), "SET 写的是旧值 + 1");
    assert_eq!(params[5], json!(1), "接着是主键");
    assert_eq!(params[6], json!(5), "WHERE 比对的是旧值");
    assert!(
        sql.ends_with(r#"WHERE "id" = ? AND "version" = ?"#),
        "版本条件必须在 WHERE 里、且在语句末尾: {sql}"
    );
}

/// 影响 0 行 = 版本不匹配（别人先改了）→ **必须是 OptimisticLockConflict**，
/// 不能退化成 NotFound —— 两者对调用方的处理完全不同（重试 vs 报错）。
///
/// 对照组（无 version 的实体报 NotFound）见 `tests.rs` 的
/// `update_reports_missing_row` —— 两条合起来才排得出「0 行的语义取决于 flags」。
#[tokio::test]
async fn version_mismatch_reports_conflict_not_not_found() {
    let spy = Spy {
        affected: 0,
        ..Default::default()
    };
    let e = D::update(&spy, &stored()).await.unwrap_err();
    assert!(matches!(e, OrmError::OptimisticLockConflict), "got: {e:?}");
}

// ---- 软删除 ----

#[tokio::test]
async fn delete_is_a_soft_update_not_a_delete_statement() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    D::delete_by_id(&spy, 1).await.unwrap();
    let (sql, params) = last(&spy);
    assert_eq!(
        sql, r#"UPDATE "docs" SET "deleted_at" = ? WHERE "id" = ? AND "deleted_at" IS NULL"#,
        "软删除必须是带 IS NULL 闸门的 UPDATE —— 加了闸门，重复删除才影响 0 行"
    );
    assert_eq!(params[1], json!(1), "软删除同样按主键定位");
    assert_filled_now(&params[0]);
}

#[tokio::test]
async fn hard_delete_is_still_available_explicitly() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    D::hard_delete_by_id(&spy, 1).await.unwrap();
    let (sql, params) = last(&spy);
    assert_eq!(sql, r#"DELETE FROM "docs" WHERE "id" = ?"#);
    assert_eq!(params, vec![json!(1)]);
}

/// 重复删除同一行 → 0 行 → `NotFound`（而不是把 `deleted_at` 覆盖成新时刻，
/// 那会让「何时删的」失真）。
#[tokio::test]
async fn deleting_an_already_soft_deleted_row_reports_missing() {
    let spy = Spy {
        affected: 0,
        ..Default::default()
    };
    let e = D::delete_by_id(&spy, 1).await.unwrap_err();
    assert!(matches!(e, OrmError::NotFound), "got: {e:?}");
}

/// **对照**：同一套断言在**没有** flags 的实体上必须给出相反的结果 ——
/// 三条自动行为都只由 `EntityMeta.flags` 驱动。少了这条，把软删除写成
/// 「永远发 UPDATE」也能让上面几条绿。
#[tokio::test]
async fn an_entity_without_flags_keeps_the_plain_behaviour() {
    let spy = Spy {
        affected: 1,
        ..Default::default()
    };
    U::delete_by_id(&spy, 1).await.unwrap();
    let (sql, _) = last(&spy);
    assert_eq!(sql, r#"DELETE FROM "users" WHERE "id" = ?"#);
}

// ---- 真库上的端到端 ----

/// 三条自动行为在**真数据库**（内存 SQLite）上跑通。
///
/// 上面那些整串 `assert_eq!` 证明的是「我们发的是这条语句」，证明不了「这条语句
/// 真库会收、而且真的这么做了」—— Task 12 实测过：把 ORDER BY 提到 WHERE 之前
/// （真库必拒），29 条测试里 28 条照绿。这条补上那个洞。
#[tokio::test]
async fn auto_behaviour_round_trips_on_a_real_database() {
    let db = mem_sqlite("auto_behaviour").await;
    db.execute(
        "CREATE TABLE docs (\
         id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, created_at TEXT, \
         updated_at TEXT, deleted_at TEXT, version INTEGER NOT NULL)",
    )
    .await
    .unwrap();

    let id = D::insert(&db, &blank()).await.unwrap();
    let mut got = D::find_by_id(&db, id)
        .await
        .unwrap()
        .expect("刚插入的行必须查得到");
    let created = got.created_at.expect("insert 必须填 created_at");
    // `created_at` 必须**落库**成 RFC3339 且接近此刻、`updated_at` 同款。
    assert!((OffsetDateTime::now_utc() - created).abs() < Duration::minutes(1));
    assert!(got.updated_at.is_some(), "insert 必须填 updated_at");

    // 乐观锁：拿一个**过期的**版本号去写 → 真库影响 0 行 → 冲突
    got.version += 1;
    got.name = "stale".into();
    let e = D::update(&db, &got).await.unwrap_err();
    assert!(matches!(e, OrmError::OptimisticLockConflict), "got: {e:?}");

    // 拿当前版本号去写 → 成功，且库里 version 真的 +1
    got.version -= 1;
    // 故意塞一个 2020 的旧值：update 必须把它覆盖成此刻（更新路径的 "always" 半边）
    let old = time::macros::datetime!(2020-01-01 00:00:00 UTC);
    got.updated_at = Some(old);
    D::update(&db, &got).await.unwrap();
    let bumped = D::find_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(bumped.version, got.version + 1, "version 应在库里 +1");
    assert_ne!(
        bumped.updated_at,
        Some(old),
        "update 必须在库里刷新 updated_at"
    );
    let refreshed = bumped.updated_at.expect("update 后 updated_at 不该是 NULL");
    assert!((OffsetDateTime::now_utc() - refreshed).abs() < Duration::minutes(1));
    assert_eq!(
        bumped.created_at, got.created_at,
        "update 不得动 created_at（事实记录）"
    );

    // 软删除：语句被真库接受、行还在、但查询路径查不到了
    D::delete_by_id(&db, id).await.unwrap();
    assert!(
        D::find_by_id(&db, id).await.unwrap().is_none(),
        "软删除后默认查询必须查不到"
    );
    let rows = db
        .query(&format!("SELECT deleted_at FROM docs WHERE id = {id}"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "软删除是 UPDATE —— 行必须还在表里");
    let deleted_at = rows[0].get("deleted_at").and_then(|v| v.as_str());
    assert!(deleted_at.is_some(), "deleted_at 应被置成时刻");

    // 重复删除 → 真库影响 0 行（`deleted_at IS NULL` 闸门）→ NotFound，
    // 而不是把删除时刻刷新掉。
    let e = D::delete_by_id(&db, id).await.unwrap_err();
    assert!(matches!(e, OrmError::NotFound), "got: {e:?}");
    let again = db
        .query(&format!("SELECT deleted_at FROM docs WHERE id = {id}"))
        .await
        .unwrap();
    assert_eq!(
        again[0].get("deleted_at"),
        rows[0].get("deleted_at"),
        "删除时刻不得被第二次删除刷新"
    );

    // 物理删除仍然可用
    D::hard_delete_by_id(&db, id).await.unwrap();
    let left = db
        .query(&format!("SELECT id FROM docs WHERE id = {id}"))
        .await
        .unwrap();
    assert!(left.is_empty(), "hard_delete_by_id 必须真的删掉行");
}

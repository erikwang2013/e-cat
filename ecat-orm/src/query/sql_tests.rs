// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `query/sql.rs` 的测试。**单独成文件**是因为它们会把 `sql.rs` 顶过
//! 项目硬规则「每个源文件 < 500 行」—— 实现与测试各自都在上限内。

use super::sql::{Built, Placeholders, build_select, render_where};
use super::{Expr, JoinType, Op, Order, Query};
use crate::dialect::lookup;
use crate::entity::{ColType, ColumnMeta, Entity, EntityFlags, EntityMeta};
use crate::error::OrmError;
use ecat_data::Dialect;
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
        name: "deleted_at",
        ty: ColType::Timestamp,
        nullable: true,
        pk: false,
        auto_increment: false,
    },
];
static META: EntityMeta = EntityMeta {
    table: "users",
    pk: "id",
    columns: &COLS,
    relations: &[],
    flags: EntityFlags {
        created_at: None,
        updated_at: None,
        soft_delete: Some("deleted_at"),
        version: None,
    },
};

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

fn sel<S>(q: &Query<U, S>, d: Dialect) -> Built {
    build_select(q, d, false)
}

// ---- SELECT 形状 ----

#[test]
fn bare_select_lists_all_columns_quoted() {
    let b = sel(&U::query(), Dialect::Postgres);
    assert!(
        b.sql
            .contains("SELECT \"id\", \"name\", \"deleted_at\" FROM \"users\""),
        "got: {}",
        b.sql
    );
    assert!(b.params.is_empty());
}

/// **软删除自动过滤**：实体标了 soft_delete 且未 with_trashed 时，
/// 查询必须自带 `WHERE deleted_at IS NULL`。
#[test]
fn soft_delete_filter_is_applied_by_default() {
    let b = sel(&U::query(), Dialect::Postgres);
    assert!(b.sql.contains("\"deleted_at\" IS NULL"), "got: {}", b.sql);
}

#[test]
fn with_trashed_removes_the_soft_delete_filter() {
    let q = U::query().with_trashed();
    let b = sel(&q, Dialect::Postgres);
    assert!(!b.sql.contains("IS NULL"), "got: {}", b.sql);
}

/// **整条 SQL 的定型断言**。上面那一串 `contains` 察觉不到
/// 「片段跑到了错误的位置」（JOIN 落到 WHERE 之后、ORDER BY 落到 LIMIT 之后
/// 都仍然「包含」）—— 这条钉住完整形状与子句顺序。
#[test]
fn full_sql_shape_is_pinned() {
    let q = U::query()
        .filter("name", Op::Eq, "alice")
        .unwrap()
        .order_by("id", Order::Desc)
        .unwrap()
        .limit(10)
        .offset(20);
    let b = sel(&q, Dialect::Postgres);
    assert_eq!(
        b.sql,
        concat!(
            "SELECT \"id\", \"name\", \"deleted_at\" FROM \"users\" ",
            "WHERE \"deleted_at\" IS NULL AND \"name\" = $1 ",
            "ORDER BY \"id\" DESC LIMIT 10 OFFSET 20"
        ),
        "got: {}",
        b.sql
    );
    assert_eq!(b.params, vec![json!("alice")]);
}

// ---- 占位符与参数 ----

#[test]
fn filter_emits_placeholders_and_params_in_order() {
    let q = U::query()
        .filter("name", Op::Eq, "alice")
        .unwrap()
        .filter("id", Op::Gt, 5)
        .unwrap();
    let b = sel(&q, Dialect::Postgres);
    // PG 是编号占位符：$1 对 name、$2 对 id —— 顺序由 filter 链决定
    assert!(b.sql.contains("\"name\" = $1"), "got: {}", b.sql);
    assert!(b.sql.contains("\"id\" > $2"), "got: {}", b.sql);
    assert_eq!(b.params, vec![json!("alice"), json!(5)]);
}

/// **同一份 Query 在 `?` 方言上必须生成 `?` 而不是 `$1`。**
#[test]
fn question_mark_dialects_get_question_marks() {
    let q = U::query().filter("name", Op::Eq, "alice").unwrap();
    let b = sel(&q, Dialect::Sqlite);
    assert!(b.sql.contains("\"name\" = ?"), "got: {}", b.sql);
    assert!(!b.sql.contains("$1"), "got: {}", b.sql);
}

/// SQL Server 是 `@Pn`。
#[test]
fn mssql_gets_at_p_n() {
    let q = U::query().filter("name", Op::Eq, "alice").unwrap();
    let b = sel(&q, Dialect::Mssql);
    assert!(b.sql.contains("[name] = @P1"), "got: {}", b.sql);
}

/// 软删除条件**不占参数位**（是字面量 IS NULL），所以用户参数仍从 1 开始编号。
/// 这条最容易错：把 IS NULL 也分配一个占位符，会让所有后续参数错位一位。
#[test]
fn soft_delete_filter_does_not_consume_a_parameter_slot() {
    let q = U::query().filter("name", Op::Eq, "alice").unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("\"name\" = $1"), "got: {}", b.sql);
    assert_eq!(b.params.len(), 1);
}

#[test]
fn in_expands_to_one_placeholder_per_value() {
    let q = U::query().filter("id", Op::In, json!([1, 2, 3])).unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("\"id\" IN ($1, $2, $3)"), "got: {}", b.sql);
    assert_eq!(b.params, vec![json!(1), json!(2), json!(3)]);
}

#[test]
fn not_in_is_negated() {
    let q = U::query().filter("id", Op::NotIn, json!([1])).unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("NOT IN"), "got: {}", b.sql);
}

#[test]
fn empty_in_list_is_a_contradiction_not_a_syntax_error() {
    // `IN ()` 是语法错误。空列表语义上恒假，写成 1 = 0。
    let q = U::query().filter("id", Op::In, json!([])).unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("1 = 0"), "got: {}", b.sql);
    assert!(b.params.is_empty(), "空 IN 不该产生参数");
}

#[test]
fn is_null_emits_no_parameter() {
    let q = U::query().filter("name", Op::IsNull, json!(null)).unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("\"name\" IS NULL"), "got: {}", b.sql);
    assert!(b.params.is_empty());
}

#[test]
fn not_null_is_negated() {
    let q = U::query().filter("name", Op::NotNull, json!(null)).unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("\"name\" IS NOT NULL"), "got: {}", b.sql);
}

#[test]
fn raw_filter_goes_in_verbatim_with_no_parameter() {
    let q = U::query().filter_raw("posts.published = 1").unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("posts.published = 1"), "got: {}", b.sql);
    assert!(b.params.is_empty());
}

// ---- ORDER BY / 分页 ----

#[test]
fn order_by_emits_direction() {
    let q = U::query().order_by("id", Order::Desc).unwrap();
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.contains("ORDER BY \"id\" DESC"), "got: {}", b.sql);
}

#[test]
fn multi_order_preserves_declaration_order() {
    let q = U::query()
        .order_by("name", Order::Asc)
        .unwrap()
        .order_by("id", Order::Desc)
        .unwrap();
    let b = sel(&q, Dialect::Postgres);
    let i_name = b.sql.find("\"name\" ASC").expect("name ASC");
    let i_id = b.sql.find("\"id\" DESC").expect("id DESC");
    assert!(i_name < i_id, "ORDER BY 顺序必须与声明一致: {}", b.sql);
}

#[test]
fn limit_and_offset_go_to_the_suffix_for_pg() {
    let q = U::query().limit(10).offset(20);
    let b = sel(&q, Dialect::Postgres);
    assert!(b.sql.ends_with(" LIMIT 10 OFFSET 20"), "got: {}", b.sql);
}

/// SQL Server 无 OFFSET 时走 TOP 前缀 —— 见「裁决 C」。
#[test]
fn mssql_limit_without_offset_uses_top_prefix() {
    let q = U::query().limit(10);
    let b = sel(&q, Dialect::Mssql);
    assert!(b.sql.starts_with("SELECT TOP (10) "), "got: {}", b.sql);
}

/// **只设 offset 不设 limit**：`offset()` 是 Task 11 已交付的公开方法，
/// `.offset(n).fetch()` 完全合法 —— 而旧实现在**四个方言上都产出真库拒收的 SQL**
/// （PG/SQLite/MySQL `LIMIT 18446744073709551615 OFFSET 20`、MSSQL
/// `FETCH NEXT 18446744073709551615 ROWS ONLY`，都超 BIGINT）。
///
/// 这里**五个方言各整串断言一次**：`contains` 式断言对「子句出现在错误位置」
/// 恒真（Task 12 实测），而这条要钉的正是「整个 LIMIT/FETCH 片段不在」。
#[test]
fn offset_without_limit_never_emits_a_sentinel_number() {
    let q = U::query().offset(20);
    let expected = [
        (
            Dialect::Standard,
            r#"SELECT "id", "name", "deleted_at" FROM "users" WHERE "deleted_at" IS NULL OFFSET 20"#,
        ),
        (
            Dialect::Sqlite,
            r#"SELECT "id", "name", "deleted_at" FROM "users" WHERE "deleted_at" IS NULL OFFSET 20"#,
        ),
        (
            Dialect::Postgres,
            r#"SELECT "id", "name", "deleted_at" FROM "users" WHERE "deleted_at" IS NULL OFFSET 20"#,
        ),
        (
            Dialect::MySql,
            "SELECT `id`, `name`, `deleted_at` FROM `users` WHERE `deleted_at` IS NULL OFFSET 20",
        ),
        (
            Dialect::Mssql,
            "SELECT [id], [name], [deleted_at] FROM [users] WHERE [deleted_at] IS NULL \
             ORDER BY (SELECT NULL) OFFSET 20 ROWS",
        ),
    ];
    for (d, want) in expected {
        let b = sel(&q, d);
        assert_eq!(b.sql, want, "{d:?} 的 SQL 形状不对");
        assert!(
            !b.sql.contains("18446744073709551615") && !b.sql.contains("9223372036854775807"),
            "{d:?} 漏出哨兵数字: {}",
            b.sql
        );
        assert!(
            !b.sql.contains("LIMIT") && !b.sql.contains("FETCH"),
            "{d:?} 不该带 LIMIT/FETCH: {}",
            b.sql
        );
        assert!(b.params.is_empty());
    }
}

// ---- JOIN（「必做之一」） ----

/// **`Left` 不能渲染成裸 `JOIN`（= `INNER`）** —— 那是不报错的错结果。
/// 三处一起改（枚举 / 字段类型 / 渲染）才是完整的：只改前两处，
/// 这条断言就会红。
#[test]
fn join_types_render_their_own_keyword() {
    assert_eq!(JoinType::Inner.as_sql(), "INNER JOIN");
    assert_eq!(JoinType::Left.as_sql(), "LEFT JOIN");
    assert_ne!(JoinType::Inner.as_sql(), JoinType::Left.as_sql());
}

/// JOIN 必须在 **FROM 之后、WHERE 之前** —— 位置错了数据库直接语法报错，
/// 但「SQL 含该子句」这类断言察觉不到。
#[test]
fn join_clause_sits_between_from_and_where() {
    let q = U::query().join(JoinType::Left, "posts", "posts.user_id = users.id");
    let b = sel(&q, Dialect::Postgres);
    let i_from = b.sql.find(" FROM ").expect("FROM");
    let i_join = b
        .sql
        .find("LEFT JOIN \"posts\" ON posts.user_id = users.id")
        .expect("JOIN");
    let i_where = b.sql.find(" WHERE ").expect("WHERE");
    assert!(
        i_from < i_join && i_join < i_where,
        "JOIN 必须在 FROM 之后、WHERE 之前: {}",
        b.sql
    );
}

/// 两次 join 的顺序与声明一致 —— 顺序错了不报错、只给错结果。
#[test]
fn joins_preserve_declaration_order() {
    let q = U::query()
        .join(JoinType::Inner, "posts", "posts.user_id = users.id")
        .join(JoinType::Left, "comments", "comments.post_id = posts.id");
    let b = sel(&q, Dialect::Postgres);
    let i_posts = b.sql.find("INNER JOIN \"posts\"").expect("posts join");
    let i_comments = b.sql.find("LEFT JOIN \"comments\"").expect("comments join");
    assert!(i_posts < i_comments, "JOIN 顺序必须与声明一致: {}", b.sql);
}

/// **COUNT 必须带同样的 JOIN** —— 否则 `total` 与分页查询的行数不符。
#[test]
fn count_query_keeps_the_joins() {
    let q = U::query().join(JoinType::Left, "posts", "posts.user_id = users.id");
    let b = build_select(&q, Dialect::Postgres, true);
    assert!(b.sql.starts_with("SELECT COUNT(*)"), "got: {}", b.sql);
    assert!(
        b.sql
            .contains("LEFT JOIN \"posts\" ON posts.user_id = users.id"),
        "got: {}",
        b.sql
    );
}

// ---- render_where（「必做之二」：Task 15 的删除路径复用它） ----

/// 没有任何条件时返回 `None` —— 调用方据此决定要不要写 `WHERE`。
#[test]
fn render_where_returns_none_when_there_is_nothing_to_filter() {
    let spec = lookup(Dialect::Postgres);
    let mut ph = Placeholders::new(spec);
    let mut params = Vec::new();
    let got = render_where(&META, &[], true, spec, &mut ph, &mut params);
    assert_eq!(got, None);
    assert!(params.is_empty());
    assert_eq!(ph.used(), 0);
}

/// 软删除闸门在 `render_where` 里**不占参数位** —— 它是字面量 `IS NULL`。
/// 占了位，用户参数会整体错位一位（`$1` 绑到 NULL 上，查询静默返回空集）。
#[test]
fn render_where_soft_delete_gate_costs_no_parameter_slot() {
    let spec = lookup(Dialect::Postgres);
    let mut ph = Placeholders::new(spec);
    let mut params = Vec::new();
    let got = render_where(&META, &[], false, spec, &mut ph, &mut params);
    assert_eq!(got.as_deref(), Some("\"deleted_at\" IS NULL"));
    assert!(params.is_empty());
    assert_eq!(ph.used(), 0, "软删除闸门不得占参数位");
}

/// **顺序效应**：调用方已用掉 2 个位时，第一个新占位符必须是 `$3` / `@P3`。
/// 函数内新建 `Placeholders` 会让编号从头开始，与调用方已生成的部分撞号。
#[test]
fn render_where_continues_placeholder_numbering_of_the_caller() {
    let filters = vec![Expr::Cmp {
        column: "name".into(),
        op: Op::Eq,
        value: json!("alice"),
    }];
    for (d, expected) in [(Dialect::Postgres, "$3"), (Dialect::Mssql, "@P3")] {
        let spec = lookup(d);
        let mut ph = Placeholders::new(spec);
        let _taken_by_the_caller = (ph.take(), ph.take());
        let mut params = Vec::new();
        let sql = render_where(&META, &filters, true, spec, &mut ph, &mut params)
            .expect("有一个 filter 就该有 WHERE");
        assert!(sql.contains(expected), "{d:?} 期望 {expected}，got: {sql}");
        assert_eq!(ph.used(), 3);
        assert_eq!(params, vec![json!("alice")]);
    }
}

/// **`build_select` 的 WHERE 必须就是 `render_where` 的产物** —— 逐字节比对。
/// Task 15 的删除路径调的是同一个函数，这条钉住「两份渲染不可能漂移」。
#[test]
fn build_select_where_is_byte_identical_to_render_where() {
    let filters = vec![
        Expr::Cmp {
            column: "name".into(),
            op: Op::Eq,
            value: json!("alice"),
        },
        Expr::In {
            column: "id".into(),
            values: vec![json!(1), json!(2)],
            negated: false,
        },
    ];
    let spec = lookup(Dialect::Postgres);
    let mut ph = Placeholders::new(spec);
    let mut direct_params = Vec::new();
    let direct = render_where(&META, &filters, false, spec, &mut ph, &mut direct_params)
        .expect("有 filter 就有 WHERE");

    let q = U::query()
        .filter("name", Op::Eq, "alice")
        .unwrap()
        .filter("id", Op::In, json!([1, 2]))
        .unwrap();
    let b = sel(&q, Dialect::Postgres);

    assert!(
        b.sql.ends_with(&format!(" WHERE {direct}")),
        "got: {}",
        b.sql
    );
    assert_eq!(b.params, direct_params);
}

// ---- COUNT ----

#[test]
fn count_reuses_the_where_clause() {
    let q = U::query().filter("name", Op::Eq, "alice").unwrap();
    let b = build_select(&q, Dialect::Postgres, true);
    assert!(b.sql.starts_with("SELECT COUNT(*)"), "got: {}", b.sql);
    assert!(b.sql.contains("\"name\" = $1"), "got: {}", b.sql);
    assert_eq!(b.params, vec![json!("alice")]);
}

/// **COUNT 必须去掉 ORDER BY / LIMIT / OFFSET**：
/// 留着 ORDER BY 让数据库白排一次序；留着 LIMIT 会**数出错误的行数**
/// （total 变成「当前页的行数」—— 静默错值，分页 UI 直接算错页数）。
#[test]
fn count_drops_order_limit_and_offset() {
    let q = U::query()
        .filter("name", Op::Eq, "alice")
        .unwrap()
        .order_by("id", Order::Desc)
        .unwrap()
        .limit(10)
        .offset(20);
    let b = build_select(&q, Dialect::Postgres, true);
    assert!(
        !b.sql.contains("ORDER BY"),
        "COUNT 不得带 ORDER BY: {}",
        b.sql
    );
    assert!(!b.sql.contains("LIMIT"), "COUNT 不得带 LIMIT: {}", b.sql);
    assert!(!b.sql.contains("OFFSET"), "COUNT 不得带 OFFSET: {}", b.sql);
}

/// COUNT 也不该带上 TOP 前缀（MSSQL）。
#[test]
fn mssql_count_has_no_top_prefix() {
    let q = U::query().limit(10);
    let b = build_select(&q, Dialect::Mssql, true);
    assert!(!b.sql.contains("TOP"), "COUNT 不得带 TOP: {}", b.sql);
}

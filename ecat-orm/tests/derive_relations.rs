// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 关联字段的派生（元数据部分；预加载执行见 relation.rs 的测试）。
//!
//! 裸关联字段（非 `Vec` / 非 `Option`）必须在**编译期**被拒。那条 `compile_fail`
//! doctest 放在 `ecat_orm::relation` 的模块文档里，**不能**放在本文件：
//! rustdoc 只处理 lib/bin 目标的文档测试，`tests/*.rs` 里的 doctest 不会被
//! cargo 执行 —— 放这里等于空验收（看着有断言，实际一次都没跑）。

use ecat_orm::relation::RelationSelector;
use ecat_orm::{Entity, OrmError, RelationKind, Row};

#[derive(Entity)]
#[entity(table = "posts")]
pub struct Post {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}

#[derive(Entity)]
#[entity(table = "profiles")]
pub struct Profile {
    #[entity(pk)]
    pub id: i64,
    pub user_id: i64,
    pub bio: String,
}

#[derive(Entity)]
#[entity(table = "comments")]
pub struct Comment {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub post_id: i64,
    pub body: String,
}

/// belongs_to 的专用实体对：目标表主键**不叫** `id`，
/// 否则「默认取本表主键」与「默认取目标表主键」两种写法都得到 `"id"`，
/// 断言就区分不出对错。
#[derive(Entity)]
#[entity(table = "tags")]
pub struct Tag {
    #[entity(pk)]
    pub code: String,
    pub label: String,
}

#[derive(Entity)]
#[entity(table = "tag_links")]
pub struct TagLink {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub tag_code: String,
    #[entity(belongs_to = "Tag", foreign_key = "tag_code")]
    pub tag: Option<Tag>,
}

#[derive(Entity)]
#[entity(table = "users")]
pub struct User {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub name: String,

    #[entity(has_many = "Post", foreign_key = "user_id")]
    pub posts: Vec<Post>,

    #[entity(has_one = "Profile", foreign_key = "user_id")]
    pub profile: Option<Profile>,

    // local_key 特意选了**非主键**的 `name`：写 `id` 的话，断言在
    // 「显式 local_key 被静默忽略」时照样通过（默认值也是 id），等于空验收。
    #[entity(has_many = "Comment", foreign_key = "post_id", local_key = "name")]
    pub comments: Vec<Comment>,
}

fn row(cols: &[&str], vals: Vec<serde_json::Value>) -> Row {
    Row::new(cols.iter().map(|s| s.to_string()).collect(), vals)
}

/// 一行 `Post`（列：id, user_id, title）。
fn post_row(id: i64, title: &str) -> Row {
    row(
        &["id", "user_id", "title"],
        vec![
            serde_json::json!(id),
            serde_json::json!(1),
            serde_json::json!(title),
        ],
    )
}

/// 一行 `Profile`（列：id, user_id, bio）—— 与 `Post` 的列不同，
/// 用同一个构造器会让 `from_row` 报 UnknownColumn("bio")。
fn profile_row(id: i64, bio: &str) -> Row {
    row(
        &["id", "user_id", "bio"],
        vec![
            serde_json::json!(id),
            serde_json::json!(1),
            serde_json::json!(bio),
        ],
    )
}

fn user_without_relations() -> User {
    User::from_row(&row(
        &["id", "name"],
        vec![serde_json::json!(1), serde_json::json!("alice")],
    ))
    .unwrap()
}

#[test]
fn relation_names_match_the_field_names() {
    let names: Vec<_> = User::META.relations.iter().map(|r| r.name).collect();
    assert_eq!(names, vec!["posts", "profile", "comments"]);
}

#[test]
fn relation_kinds_are_captured() {
    let k = |n: &str| User::META.relation(n).unwrap().kind;
    assert_eq!(k("posts"), RelationKind::HasMany);
    assert_eq!(k("profile"), RelationKind::HasOne);
    assert_eq!(k("comments"), RelationKind::HasMany);
}

#[test]
fn has_many_targets_the_right_table_and_fk() {
    let r = User::META.relation("posts").unwrap();
    assert_eq!(r.target_table, "posts");
    assert_eq!(r.foreign_key, "user_id");
    // local_key 省略时取本表主键
    assert_eq!(r.local_key, "id");
}

#[test]
fn explicit_local_key_is_honoured() {
    let r = User::META.relation("comments").unwrap();
    assert_eq!(r.foreign_key, "post_id");
    // User 的主键是 `id`，属性里写的是 `name` —— 两者不同，
    // 忽略属性而回落到默认值时这条会红。
    assert_eq!(r.local_key, "name");
}

/// `belongs_to` 的方向与 `has_many` / `has_one` 相反：本表 `foreign_key`
/// → **目标表**主键。`Tag` 的主键是 `code`、`TagLink` 的是 `id`，
/// 所以这两个候选值在断言里可区分。
#[test]
fn belongs_to_defaults_local_key_to_the_target_pk() {
    let r = TagLink::META.relation("tag").unwrap();
    assert_eq!(r.kind, RelationKind::BelongsTo);
    assert_eq!(r.target_table, "tags");
    assert_eq!(r.foreign_key, "tag_code");
    assert_eq!(
        r.local_key, "code",
        "belongs_to 省略 local_key 时取目标表主键"
    );
    assert_eq!(TagLinkRelation::Tag.name(), "tag");
}

#[test]
fn relation_fields_are_not_columns() {
    let names: Vec<_> = User::META.columns.iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["id", "name"], "关联字段不得混进列里");
}

/// 每个实体都生成自己的 `XxxRelation`，变体名是字段名的 PascalCase。
///
/// 同时证明 `RelationSelector` 的 impl 真的可用（走 trait 而不是固有方法）——
/// 预加载要靠它抹平不同实体枚举的类型差异。
#[test]
fn relation_enum_variants_are_generated() {
    fn name_of<R: RelationSelector>(r: R) -> &'static str {
        r.name()
    }
    assert_eq!(UserRelation::Posts.name(), "posts");
    assert_eq!(name_of(UserRelation::Profile), "profile");
    assert_eq!(name_of(UserRelation::Comments), "comments");
}

/// 关联目标类型必须在 derive 处可见 —— 这条测试同时证明了生成的代码
/// 引用的是目标类型的 `META` 而不是硬编码字符串。
#[test]
fn target_meta_is_reachable_from_the_relation() {
    assert_eq!(
        User::META.relation("posts").unwrap().target_table,
        Post::TABLE
    );
}

/// 未预加载时关联字段是空的 —— 「没查」与「查了没有」在批次 3 里不可区分，
/// 所以文档要写明：要区分就必须显式 `with()`。
#[test]
fn from_row_leaves_relations_empty_until_loaded() {
    let u = user_without_relations();
    assert!(u.posts.is_empty());
    assert!(u.profile.is_none());
    assert!(u.comments.is_empty());
}

/// 预加载写回：多值全收，单值只取第一行。
#[test]
fn set_relation_dispatches_by_name_and_arity() {
    let mut u = user_without_relations();

    u.set_relation("posts", vec![post_row(10, "a"), post_row(11, "b")])
        .unwrap();
    assert_eq!(u.posts.len(), 2);
    assert_eq!(u.posts[0].title, "a");
    assert_eq!(u.posts[1].id, 11);

    // 单值关联：给三行，只留第一行
    assert_eq!(
        User::META.relation("profile").unwrap().kind,
        RelationKind::HasOne
    );
    u.set_relation(
        "profile",
        vec![
            profile_row(20, "x"),
            profile_row(21, "y"),
            profile_row(22, "z"),
        ],
    )
    .unwrap();
    assert!(u.profile.is_some());
    assert_eq!(u.profile.as_ref().unwrap().id, 20);
}

/// 空 `rows` 必须**清空**字段，不是「什么都不做」。
/// 否则重新加载时前一次的旧关联会留在字段里（静默给过时数据）。
#[test]
fn set_relation_with_no_rows_clears_the_field() {
    let mut u = user_without_relations();
    u.set_relation("posts", vec![post_row(1, "old")]).unwrap();
    assert_eq!(u.posts.len(), 1);

    u.set_relation("posts", vec![]).unwrap();
    assert!(u.posts.is_empty(), "空结果必须清空残留");

    // 单值关联同理：残留的 Some 必须被清成 None。
    u.set_relation("profile", vec![profile_row(9, "old")])
        .unwrap();
    assert!(u.profile.is_some());
    u.set_relation("profile", vec![]).unwrap();
    assert!(u.profile.is_none(), "空结果必须把单值关联清成 None");
}

/// 未声明的关联名必须报错，不能静默忽略。
#[test]
fn set_relation_rejects_unknown_names() {
    let mut u = user_without_relations();
    let e = u.set_relation("nope", vec![]).unwrap_err();
    assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
}

/// 写回的目标类型必须被真正解析 —— 行里缺列时 `set_relation` 不能假装成功。
#[test]
fn set_relation_propagates_target_parse_errors() {
    let mut u = user_without_relations();
    let e = u
        .set_relation(
            "posts",
            vec![row(
                &["id", "user_id"],
                vec![serde_json::json!(1), serde_json::json!(1)],
            )],
        )
        .unwrap_err();
    assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
}

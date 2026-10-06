// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `relation` 的测试夹具：按调用顺序返回不同结果的假 executor + 测试实体。
//!
//! **单独成文件**是为了守住项目硬规则「每个源文件 < 500 行」。
//! 夹具要跨模块用（`relation::tests`），故标 `pub(super)`。

use super::*;
use crate::Entity;
use crate::entity::{ColType, ColumnMeta, EntityFlags, EntityMeta, RelationMeta};
use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsError};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;

/// 按调用顺序返回预设行、并记录 `(sql, params)` 的假 executor。
///
/// 与 `crud::fixtures::Spy` 的区别正在于「按顺序」：Spy 每次查询都返回同一份行，
/// 答不了「主体查询与关联查询结果不同」—— 而数 N+1 正需要这个。
pub(super) struct Queue {
    responses: Mutex<VecDeque<Vec<Row>>>,
    calls: Mutex<Vec<(String, Vec<Value>)>>,
    dialect: Dialect,
}

impl Queue {
    pub(super) fn new(dialect: Dialect) -> Self {
        Self {
            responses: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            dialect,
        }
    }

    /// 追加「下一次查询的返回值」。
    pub(super) fn push(&self, rows: Vec<Row>) {
        self.responses.lock().unwrap().push_back(rows);
    }

    /// 已收到的全部 `(sql, params)`。**数 N+1 就是数它的长度。**
    pub(super) fn calls(&self) -> Vec<(String, Vec<Value>)> {
        self.calls.lock().unwrap().clone()
    }

    /// 下一次查询的返回值（队列空 → 空结果集）。
    fn next_rows(&self) -> Vec<Row> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default()
    }
}

#[async_trait]
impl SqlExecutor for Queue {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        Ok(0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.calls
            .lock()
            .unwrap()
            .push((sql.to_string(), Vec::new()));
        Ok(self.next_rows())
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.calls
            .lock()
            .unwrap()
            .push((sql.to_string(), params.to_vec()));
        Ok(self.next_rows())
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

// ---- 派生实体 ----

#[derive(Debug, Entity)]
#[entity(table = "rb_posts")]
pub(super) struct RbPost {
    #[entity(pk)]
    pub(super) id: i64,
    pub(super) user_id: i64,
    pub(super) title: String,
}

#[derive(Debug, Entity)]
#[entity(table = "rb_profiles")]
pub(super) struct RbProfile {
    #[entity(pk)]
    pub(super) id: i64,
    pub(super) user_id: i64,
    pub(super) bio: String,
}

#[derive(Debug, Entity)]
#[entity(table = "rb_comments")]
pub(super) struct RbComment {
    #[entity(pk)]
    pub(super) id: i64,
    pub(super) post_id: String,
    pub(super) body: String,
}

/// belongs_to 的目标：主键**不叫** `id`，否则「匹配哪一列」写反了也看不出来。
#[derive(Debug, Entity)]
#[entity(table = "rb_tags")]
pub(super) struct RbTag {
    #[entity(pk)]
    pub(super) code: String,
    pub(super) label: String,
}

#[derive(Debug, Entity)]
#[entity(table = "rb_users")]
pub(super) struct RbUser {
    #[entity(pk, auto_increment)]
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) tag_code: String,

    #[entity(has_many = "RbPost", foreign_key = "user_id")]
    pub(super) posts: Vec<RbPost>,

    #[entity(has_one = "RbProfile", foreign_key = "user_id")]
    pub(super) profile: Option<RbProfile>,

    // local_key 特意选了**非主键**的 `name`：主体键走 `to_values()` 取值
    // 而不是主键捷径，顺带把「主体键不一定是主键」这条路径也钉住。
    #[entity(has_many = "RbComment", foreign_key = "post_id", local_key = "name")]
    pub(super) comments: Vec<RbComment>,

    #[entity(belongs_to = "RbTag", foreign_key = "tag_code")]
    pub(super) tag: Option<RbTag>,
}

pub(super) fn row(cols: &[&str], vals: Vec<Value>) -> Row {
    Row::new(cols.iter().map(|s| s.to_string()).collect(), vals)
}

pub(super) fn rb_user_row(id: i64, name: &str, tag_code: &str) -> Row {
    row(
        &["id", "name", "tag_code"],
        vec![json!(id), json!(name), json!(tag_code)],
    )
}

pub(super) fn rb_post_row(id: i64, user_id: i64, title: &str) -> Row {
    row(
        &["id", "user_id", "title"],
        vec![json!(id), json!(user_id), json!(title)],
    )
}

pub(super) fn rb_profile_row(id: i64, user_id: i64, bio: &str) -> Row {
    row(
        &["id", "user_id", "bio"],
        vec![json!(id), json!(user_id), json!(bio)],
    )
}

pub(super) fn rb_comment_row(id: i64, post_id: &str, body: &str) -> Row {
    row(
        &["id", "post_id", "body"],
        vec![json!(id), json!(post_id), json!(body)],
    )
}

pub(super) fn rb_tag_row(code: &str, label: &str) -> Row {
    row(&["code", "label"], vec![json!(code), json!(label)])
}

// ---- 只用手写实体能观察到的行为所需的探针 ----

/// 探针实体：记录每次 `set_relation` 收到的**行数**。
///
/// 派生实体的单值 setter 会**静默**只留第一行，从外面看不出加载器给了它几行 ——
/// 「单值关联只交第一行」这条只能在能自己数行数的实现上观察。
static PROBE_COLS: [ColumnMeta; 1] = [ColumnMeta {
    name: "key",
    ty: ColType::Text,
    nullable: true,
    pk: true,
    auto_increment: false,
}];
static PROBE_RELS: [RelationMeta; 1] = [RelationMeta {
    name: "one",
    kind: RelationKind::HasOne,
    target_table: "probe_targets",
    foreign_key: "owner",
    local_key: "key",
}];
static PROBE_META: EntityMeta = EntityMeta {
    table: "probe",
    pk: "key",
    columns: &PROBE_COLS,
    relations: &PROBE_RELS,
    flags: EntityFlags::NONE,
};

#[derive(Default)]
pub(super) struct Probe {
    key: Option<String>,
    /// 每次写回的 `(关联名, 行数)`。
    pub(super) writes: Vec<(String, usize)>,
}

impl Entity for Probe {
    const TABLE: &'static str = "probe";
    const PK: &'static str = "key";
    const META: &'static EntityMeta = &PROBE_META;

    fn from_row(_row: &Row) -> Result<Self, OrmError> {
        // 本实体只当主体，从不做关联目标 —— from_row 不会被走到。
        Ok(Self::default())
    }

    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![("key", json!(self.key))]
    }

    fn pk_value(&self) -> Value {
        json!(self.key)
    }

    fn set_relation(&mut self, name: &str, rows: Vec<Row>) -> Result<(), OrmError> {
        self.writes.push((name.to_string(), rows.len()));
        Ok(())
    }
}

pub(super) fn probe(key: &str) -> Probe {
    Probe {
        key: Some(key.to_string()),
        writes: Vec::new(),
    }
}

pub(super) fn probe_row(owner: &str) -> Row {
    row(&["owner"], vec![json!(owner)])
}

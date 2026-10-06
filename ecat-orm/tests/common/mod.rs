// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 集成测试的共用夹具：实体定义、临时 SQLite 文件库、计数执行器。
//! 独立成 `tests/common/mod.rs` 是为了守住「每个源文件 < 500 行」——
//! `sqlite_e2e.rs` 只放 spec §12 第 4 条那条链路的断言。
//!
//! 背景（多连接）：`sqlite::memory:` 下池里**每条连接是各自独立的空库**，前一条
//! 建的表后续连接看不到（实测：事务占住一条连接后再查 → 30s 查询超时）。
//! `?mode=memory&cache=shared` 也在**事务持写锁时把第二条连接卡死**（实测同上）。
//! 所以这里用**临时文件**（`mode=rwc`）：同一文件、多连接、无共享缓存锁 ——
//! 实测 `pool_size=2` 且第二条连接读得到第一条建的表与已提交的数据。

use ecat_data::{Dialect, RdbmsError, Row, SqlExecutor};
use ecat_data_sqlx::SqlxClient;
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Entity, Migrator, create_table};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use time::OffsetDateTime;

// ---- 实体定义（`#[derive(Entity)]` 的部分，spec §12-4 第一环）----

#[derive(Entity, Debug, Clone)]
#[entity(table = "e2e_users")]
pub struct User {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    #[entity(created_at)]
    pub created_at: Option<OffsetDateTime>,
    #[entity(updated_at)]
    pub updated_at: Option<OffsetDateTime>,
    #[entity(soft_delete)]
    pub deleted_at: Option<OffsetDateTime>,
    #[entity(version)]
    pub version: i64,
    #[entity(has_many = "Post", foreign_key = "user_id")]
    pub posts: Vec<Post>,
}

#[derive(Entity, Debug, Clone)]
#[entity(table = "e2e_posts")]
pub struct Post {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}

pub fn user(name: &str) -> User {
    User {
        id: 0,
        name: name.into(),
        email: None,
        created_at: None,
        updated_at: None,
        deleted_at: None,
        version: 1,
        posts: Vec::new(),
    }
}

pub fn post(user_id: i64, title: &str) -> Post {
    Post {
        id: 0,
        user_id,
        title: title.into(),
    }
}

// ---- 临时库 ----

static NEXT: AtomicU64 = AtomicU64::new(0);

/// 每个测试一个**独立文件**：并发用例之间不共享库，也不共享锁。
pub fn db_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ecat-orm-e2e-{}-{tag}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// 连一个**空**的临时文件库（建表由测试自己走 `Migrator` —— 手写 DDL 的话
/// 迁移系统本身就没被测到）。返回 `(db, path)`，`path` 供测试收尾删除。
pub async fn empty_db(tag: &str) -> (SqlxClient, PathBuf) {
    let path = db_path(tag);
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite:{}?mode=rwc", path.display());
    let db = SqlxClient::connect(&url)
        .await
        .expect("connect to the temp sqlite file");
    (db, path)
}

/// 迁移列表：本测试链路用的两张表。反向走 `drop_table`（`create_table` 自己
/// **不带**反向 SQL —— 不带就 `down` 报 `MigrationIrreversible`，那是 Task 17
/// 的既定语义，不是缺陷）。
pub fn migrations<'a>(db: &'a SqlxClient) -> Migrator<'a, SqlxClient> {
    Migrator::new(db)
        .add(
            "001_users",
            create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)),
        )
        .add(
            "002_posts",
            create_table::<Post>().with_reverse(|d| drop_table_sql(Post::META, d)),
        )
}

/// 连一个空库并把两张表建好（供不专门测迁移的用例）。
pub async fn migrated_db(tag: &str) -> (SqlxClient, PathBuf) {
    let (db, path) = empty_db(tag).await;
    migrations(&db).run().await.expect("migrate the temp db");
    (db, path)
}

pub fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
}

/// 计数的中间层：**记录每一条语句与它绑定的参数**，其余原样转发给真客户端。
///
/// 两处用到：
/// - 数 N+1 就是数它的长度 —— 「3 个主体 + 关联 = 4 条」与「= 2 条」在这里是两种
///   可区分的观测结果，而只看返回数据的断言对两者都恒真
/// - 时间列的**写入侧**归一化只在绑定参数上可见（SQLite 的读路径会把任何
///   偏移量文本重新格式化回 UTC，见 ⑦ 的说明）
pub struct Counting<'a> {
    inner: &'a SqlxClient,
    seen: Mutex<Vec<(String, Vec<serde_json::Value>)>>,
}

impl<'a> Counting<'a> {
    pub fn new(inner: &'a SqlxClient) -> Self {
        Self {
            inner,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, sql: &str, params: &[serde_json::Value]) {
        self.seen
            .lock()
            .unwrap()
            .push((sql.to_string(), params.to_vec()));
    }

    pub fn calls(&self) -> Vec<(String, Vec<serde_json::Value>)> {
        self.seen.lock().unwrap().clone()
    }

    pub fn statements(&self) -> Vec<String> {
        self.calls().into_iter().map(|(sql, _)| sql).collect()
    }
}

#[async_trait::async_trait]
impl SqlExecutor for Counting<'_> {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.record(sql, &[]);
        self.inner.execute(sql).await
    }
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.record(sql, &[]);
        self.inner.query(sql).await
    }
    async fn execute_with(&self, sql: &str, p: &[serde_json::Value]) -> Result<u64, RdbmsError> {
        self.record(sql, p);
        self.inner.execute_with(sql, p).await
    }
    async fn query_with(&self, sql: &str, p: &[serde_json::Value]) -> Result<Vec<Row>, RdbmsError> {
        self.record(sql, p);
        self.inner.query_with(sql, p).await
    }
    // `query_write` 不覆写：默认实现委托 `query_with`，照样计数（覆写一份
    // 等于给自己留一条不计数的小路）。
    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }
}

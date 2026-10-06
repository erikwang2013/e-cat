// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `ecat-orm` —— e-cat 的 ORM。实体宏、查询构建器、CRUD、关联预加载、迁移。
//!
//! 数据操作全部取 `&impl SqlExecutor`（`find_by_id` / `find_all` / `update` /
//! `delete_by_id` / `insert` / `save`），因此**客户端与 `Transaction` 通吃**：
//!
//! ```ignore
//! User::find_by_id(&db, 1).await?;      // db: SqlxClient
//! let tx = db.transaction().await?;
//! User::update(&tx, &user).await?;      // tx: Transaction —— 读/改/删/插同一个 API
//! User::insert(&tx, &user).await?;      // 事务里也能插入（spec §5.4）
//! tx.commit().await?;
//! ```
//!
//! MySQL 的主键回填是两步式（`INSERT` 然后 `SELECT LAST_INSERT_ID()`），而
//! `LAST_INSERT_ID()` 是**连接作用域**的，两条语句必须落在同一条连接上，
//! 否则会取回别的会话刚插入的 id（静默错值）。这件事由
//! [`SqlExecutor::execute_then_query`] 承担：传客户端时它开一个事务把两条语句
//! 包起来并提交；传 `Transaction` 时它直接在**调用方的事务**里跑两条，
//! 不另开也不提交 —— 所以 `&tx` 与 `&client` 都收。
//!
//! 用法见 `docs/api.md` 的 ORM 段。

// 让派生宏生成的绝对路径 `::ecat_orm::…` 在本 crate 内部（含测试）也能解析。
// 派生宏无法知道用户在哪个 crate，只能生成绝对路径；没有这一行，本 crate
// 自己的 `#[derive(Entity)]` 测试会报 "use of undeclared crate or module"。
// serde 用同样的手法（`extern crate self as serde;`）。
extern crate self as ecat_orm;

mod crud;
pub mod dialect;
mod entity;
mod error;
pub mod query;
pub mod relation;
mod time;
pub mod value;

pub use ecat_data::{Row, SqlExecutor};
pub use ecat_orm_derive::Entity;
pub use entity::{ColType, ColumnMeta, EntityFlags, EntityMeta, RelationKind, RelationMeta};
pub use error::OrmError;

/// 重导出 `serde_json`：派生宏生成的代码要写 `::ecat_orm::serde_json::Value`。
/// 不重导的话，每个用户 crate 都得自己把 `serde_json` 加进依赖才能用 ORM —— 而
/// 它其实只是 ORM 公开签名里的类型（`to_values` 的返回类型）。
pub use serde_json;

/// 实体 trait。与派生宏同名，`use ecat_orm::{Entity, ...}` 一次拿到两者。
pub use entity::Entity;

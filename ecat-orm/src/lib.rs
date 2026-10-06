// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `ecat-orm` —— e-cat 的 ORM。实体宏、查询构建器、CRUD、关联预加载、迁移。
//!
//! 所有数据操作取 `&impl SqlExecutor`，因此**客户端与 `Transaction` 通吃**：
//!
//! ```ignore
//! User::insert(&db, &user).await?;      // db: SqlxClient
//! let tx = db.transaction().await?;
//! User::insert(&tx, &user).await?;      // tx: Transaction —— 同一个 API
//! tx.commit().await?;
//! ```
//!
//! 用法见 `docs/api.md` 的 ORM 段。

// 让派生宏生成的绝对路径 `::ecat_orm::…` 在本 crate 内部（含测试）也能解析。
// 派生宏无法知道用户在哪个 crate，只能生成绝对路径；没有这一行，本 crate
// 自己的 `#[derive(Entity)]` 测试会报 "use of undeclared crate or module"。
// serde 用同样的手法（`extern crate self as serde;`）。
extern crate self as ecat_orm;

pub mod dialect;
mod entity;
mod error;
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

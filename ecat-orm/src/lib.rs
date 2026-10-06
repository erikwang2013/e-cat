// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-orm` —— e-cat 的 ORM。见 `docs/api.md` 的 ORM 段。

mod entity;
mod error;
mod time;
pub mod value;

// `Entity` 同时是 trait（类型命名空间）与派生宏（宏命名空间）—— 两者可共存，
// 与 serde 的 `Serialize` 同一模式。
pub use ecat_orm_derive::Entity;
pub use entity::{
    ColType, ColumnMeta, Entity, EntityFlags, EntityMeta, RelationKind, RelationMeta,
};
pub use error::OrmError;

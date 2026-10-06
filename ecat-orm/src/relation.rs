// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 关联的静态描述与选择器。
//!
//! 关联字段的容器类型是**硬约束**：`Vec<T>`（has_many）或 `Option<T>`
//! （has_one / belongs_to）。裸实体字段在**编译期**就被拒 —— 它无法表达
//! 「没查到」，给它一个默认实例等于凭空造一条不存在的数据：
//!
//! ```compile_fail
//! use ecat_orm::Entity;
//!
//! #[derive(Entity)]
//! struct Other {
//!     #[entity(pk)]
//!     id: i64,
//! }
//!
//! #[derive(Entity)]
//! struct Bad {
//!     #[entity(pk)]
//!     id: i64,
//!     // 裸类型：没有「未加载」的表达
//!     #[entity(has_one = "Other", foreign_key = "other_id")]
//!     other: Other,
//! }
//!
//! fn main() {}
//! ```
//!
//! 同一段代码换成 `Option<Other>` 就合法。下面这个 doctest 是上面那条的
//! **对照组**：它证明上面失败的原因是裸类型本身，而不是别的语法问题。
//!
//! ```
//! use ecat_orm::Entity;
//!
//! #[derive(Entity)]
//! struct Other {
//!     #[entity(pk)]
//!     id: i64,
//! }
//!
//! #[derive(Entity)]
//! struct Good {
//!     #[entity(pk)]
//!     id: i64,
//!     #[entity(has_one = "Other", foreign_key = "other_id")]
//!     other: Option<Other>,
//! }
//!
//! fn main() {}
//! ```

/// 由 `#[derive(Entity)]` 为每个实体生成的 `XxxRelation` 枚举实现它。
///
/// 存在的理由：`Query::with(..)` 要接受**任意实体**的关联枚举，而它们在编译期
/// 是不同类型。这个 trait 把它们的差异收敛到「能报出自己的关联名」这一点上，
/// 预加载随后按名字去 `EntityMeta` 查细节。
///
/// 它不是 trait object：`Copy` 超 trait 已蕴含 `Sized`，`name` 又按值取
/// `self`。泛型的静态分派即可 —— 一次查询只处理一个实体的关联枚举。
pub trait RelationSelector: Copy + Send + Sync {
    fn name(self) -> &'static str;
}

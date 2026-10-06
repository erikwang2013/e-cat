// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-orm` 的派生宏。请勿直接依赖本 crate —— 经由 `ecat-orm` 重导出使用。

use proc_macro::TokenStream;

/// 为结构体生成 [`ecat_orm::Entity`] 实现与实体元数据。
#[proc_macro_derive(Entity, attributes(entity))]
pub fn derive_entity(_input: TokenStream) -> TokenStream {
    TokenStream::new() // Task 5 实现
}

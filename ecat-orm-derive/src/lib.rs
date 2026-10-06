// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-orm` 的派生宏。请勿直接依赖本 crate —— 经由 `ecat-orm` 重导出使用。

use proc_macro::TokenStream;
use syn::DeriveInput;

mod attr;
mod expand;

/// 为结构体生成 [`ecat_orm::Entity`] 实现与实体元数据。
///
/// 属性文法见 `docs/api.md` 的 ORM 段。
#[proc_macro_derive(Entity, attributes(entity))]
pub fn derive_entity(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand::expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

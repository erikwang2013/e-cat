// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `#[entity(...)]` 属性解析。

use syn::Attribute;
use syn::Result;

/// 容器级属性（结构体上）。
#[derive(Default)]
pub struct ContainerAttrs {
    pub table: Option<String>,
}

/// 字段级属性。
#[derive(Default)]
pub struct FieldAttrs {
    pub column: Option<String>,
    pub pk: bool,
    pub auto_increment: bool,
    pub created_at: bool,
    pub updated_at: bool,
    pub soft_delete: bool,
    pub version: bool,
    pub relation: Option<Relation>,
}

/// 关联字段。
pub struct Relation {
    pub kind: RelationKind,
    /// 目标实体类型，如 `Post`。生成代码里会写成 `Post::TABLE`，
    /// 因此**该类型必须在 derive 处的作用域内可见**。
    pub target: syn::Path,
    pub foreign_key: String,
    pub local_key: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    HasMany,
    HasOne,
    BelongsTo,
}

impl RelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HasMany => "has_many",
            Self::HasOne => "has_one",
            Self::BelongsTo => "belongs_to",
        }
    }
}

fn entity_attrs(attrs: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attrs.iter().filter(|a| a.path().is_ident("entity"))
}

pub fn parse_container(attrs: &[Attribute]) -> Result<ContainerAttrs> {
    let mut out = ContainerAttrs::default();
    for a in entity_attrs(attrs) {
        a.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                out.table = Some(lit.value());
                Ok(())
            } else {
                Err(meta.error(
                    "unknown `#[entity(...)]` on a struct; only `table = \"...\"` is supported here",
                ))
            }
        })?;
    }
    Ok(out)
}

pub fn parse_field(attrs: &[Attribute]) -> Result<FieldAttrs> {
    let mut out = FieldAttrs::default();
    for a in entity_attrs(attrs) {
        a.parse_nested_meta(|meta| {
            let p = &meta.path;
            if p.is_ident("column") {
                out.column = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if p.is_ident("pk") {
                out.pk = true;
            } else if p.is_ident("auto_increment") {
                // 自增必然是主键；显式写出 pk 不报错，但语义上等价。
                out.auto_increment = true;
                out.pk = true;
            } else if p.is_ident("created_at") {
                out.created_at = true;
            } else if p.is_ident("updated_at") {
                out.updated_at = true;
            } else if p.is_ident("soft_delete") {
                out.soft_delete = true;
            } else if p.is_ident("version") {
                out.version = true;
            } else if p.is_ident("has_many") || p.is_ident("has_one") || p.is_ident("belongs_to") {
                let kind = if p.is_ident("has_many") {
                    RelationKind::HasMany
                } else if p.is_ident("has_one") {
                    RelationKind::HasOne
                } else {
                    RelationKind::BelongsTo
                };
                let target_lit: syn::LitStr = meta.value()?.parse()?;
                let target = target_lit.parse::<syn::Path>().map_err(|e| {
                    syn::Error::new(
                        target_lit.span(),
                        format!("expected an entity type path: {e}"),
                    )
                })?;
                // foreign_key 是必填的。parse_nested_meta 在这个位置拿不到逗号后的内容，
                // 需要一个内部循环：
                let mut foreign_key = None;
                let mut local_key = None;
                while meta.input.peek(syn::Token![,]) {
                    let _: syn::Token![,] = meta.input.parse()?;
                    let id: syn::Ident = meta.input.parse()?;
                    let _: syn::Token![=] = meta.input.parse()?;
                    let lit: syn::LitStr = meta.input.parse()?;
                    if id == "foreign_key" {
                        foreign_key = Some(lit.value());
                    } else if id == "local_key" {
                        local_key = Some(lit.value());
                    } else {
                        return Err(syn::Error::new(
                            id.span(),
                            "expected `foreign_key` or `local_key`",
                        ));
                    }
                }
                let foreign_key = foreign_key.ok_or_else(|| {
                    syn::Error::new(
                        target_lit.span(),
                        format!("`{}` requires `foreign_key = \"...\"`", kind.as_str()),
                    )
                })?;
                out.relation = Some(Relation {
                    kind,
                    target,
                    foreign_key,
                    local_key,
                });
            } else {
                return Err(meta.error("unknown `#[entity(...)]` attribute on a field"));
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// 结构体名 → snake_case（`UserProfile` → `user_profile`）。
/// 只在表名省略时用作默认值。
pub fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// 字段名 → PascalCase（`posts` → `Posts`，`user_profile` → `UserProfile`）。
/// 用于生成 `XxxRelation` 的变体名。
pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

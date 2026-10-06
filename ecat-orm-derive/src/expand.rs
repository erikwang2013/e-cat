// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::DeriveInput;
use syn::Fields;
use syn::Result;
use syn::spanned::Spanned;

use crate::attr;

/// 一个列字段的全部生成所需信息。
struct Column {
    ident: syn::Ident,
    /// **完整**字段类型（`Option<T>` 就是 `Option<T>`，不拆成 `T`）。
    /// `from_row` 与 `COL_TYPE` 都直接用它 —— `Option<T>: ColumnValue` 的
    /// `COL_TYPE` 就等于 `T::COL_TYPE`，而拆开会让 `from_row_col::<T>` 在
    /// NULL 上报错，`Option` 字段拿不回 `None`。
    ty: syn::Type,
    name: String,
    nullable: bool,
    pk: bool,
    auto_increment: bool,
}

/// 字段类型是不是 `Option<..>` —— 只用来定元数据里的 `nullable`。
fn is_option(ty: &syn::Type) -> bool {
    if let syn::Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && args.args.len() == 1
        && let Some(syn::GenericArgument::Type(_)) = args.args.first()
    {
        return true;
    }
    false
}

/// 一个关联字段的全部生成所需信息。
struct RelationOut {
    /// `XxxRelation` 的变体名（字段名的 PascalCase）。
    variant: syn::Ident,
    /// 字段名 —— `set_relation` 的写回目标。
    ident: syn::Ident,
    /// 关联名，与 `EntityMeta::relations` 里的 `name` 对应。
    name: String,
    kind: attr::RelationKind,
    /// 目标实体类型。生成代码里以 `<T as Entity>::TABLE` / `::PK` 取值，
    /// 因此该类型必须在 derive 处可见。
    target: syn::Path,
    foreign_key: String,
    /// 已展开成 token 的 local_key：属性省略时是 `<T as Entity>::PK`
    /// （本表或目标表的主键，取决于关联方向）。
    local_key: TokenStream,
    /// 「未预加载」状态下字段的值，供 `from_row` 使用。
    default: TokenStream,
}

/// 关联字段在「未预加载」状态下的值。
///
/// `Vec<T>` → 空向量，`Option<T>` → `None`。其它类型直接报错 ——
/// 一个裸的 `Post` 字段**必须**有一个值，给它一个默认实体等于凭空造了一条
/// 不存在的数据。要么用 `Option`，要么用 `Vec`，没有第三条路。
fn relation_default(ty: &syn::Type, span: proc_macro2::Span) -> Result<TokenStream> {
    if let syn::Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
    {
        if seg.ident == "Vec" {
            return Ok(quote!(::std::vec::Vec::new()));
        }
        if seg.ident == "Option" {
            return Ok(quote!(::core::option::Option::None));
        }
    }
    Err(syn::Error::new(
        span,
        "a relation field must be `Vec<Target>` (has_many) or `Option<Target>` (has_one/belongs_to)",
    ))
}

pub fn expand(input: DeriveInput) -> Result<TokenStream> {
    let container = attr::parse_container(&input.attrs)?;
    let struct_ident = input.ident.clone();
    let table = container
        .table
        .unwrap_or_else(|| attr::to_snake_case(&struct_ident.to_string()));

    let named = match &input.data {
        syn::Data::Struct(s) => match &s.fields {
            Fields::Named(n) => n,
            _ => {
                return Err(syn::Error::new(
                    input.span(),
                    "Entity requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                input.span(),
                "Entity can only be derived for structs",
            ));
        }
    };

    let mut columns: Vec<Column> = Vec::new();
    let mut relations: Vec<RelationOut> = Vec::new();
    let mut flags = FlagsAcc::default();
    let mut pk_ident: Option<syn::Ident> = None;
    let mut pk_name: Option<String> = None;

    for field in &named.named {
        let ident = field.ident.clone().expect("named field");
        let fa = attr::parse_field(&field.attrs)?;

        if let Some(rel) = &fa.relation {
            let target = rel.target.clone();
            // HasMany/HasOne 的 local_key 省略时是**本表**主键；
            // BelongsTo 的方向相反，省略时是**目标表**主键。
            let local_key = match (&rel.local_key, rel.kind) {
                (Some(k), _) => quote!(#k),
                (None, attr::RelationKind::BelongsTo) => {
                    quote!(<#target as ::ecat_orm::Entity>::PK)
                }
                (None, _) => quote!(<#struct_ident as ::ecat_orm::Entity>::PK),
            };
            let name = ident.to_string();
            relations.push(RelationOut {
                variant: format_ident!("{}", attr::to_pascal_case(&name)),
                ident,
                name,
                kind: rel.kind,
                target,
                foreign_key: rel.foreign_key.clone(),
                local_key,
                default: relation_default(&field.ty, field.span())?,
            });
            continue; // 关联字段不是列
        }

        let name = fa.column.clone().unwrap_or_else(|| ident.to_string());
        let nullable = is_option(&field.ty);

        if fa.created_at {
            flags.created_at = Some(name.clone());
        }
        if fa.updated_at {
            flags.updated_at = Some(name.clone());
        }
        if fa.soft_delete {
            flags.soft_delete = Some(name.clone());
        }
        if fa.version {
            flags.version = Some(name.clone());
        }
        if fa.pk {
            if pk_ident.is_some() {
                return Err(syn::Error::new(
                    field.span(),
                    "multiple `pk` fields; ecat-orm does not support composite primary keys",
                ));
            }
            pk_ident = Some(ident.clone());
            pk_name = Some(name.clone());
        }

        columns.push(Column {
            ident,
            ty: field.ty.clone(),
            name,
            nullable,
            pk: fa.pk,
            auto_increment: fa.auto_increment,
        });
    }

    let pk_ident = pk_ident.ok_or_else(|| {
        syn::Error::new(
            struct_ident.span(),
            "no `#[entity(pk)]` field; every entity needs a single-column primary key",
        )
    })?;
    let pk_name = pk_name.expect("set alongside pk_ident");

    // 把这些绑定成 Vec<lit>，供 quote 的重复插值使用。
    let col_names: Vec<_> = columns.iter().map(|c| c.name.as_str()).collect();
    let col_tys: Vec<_> = columns.iter().map(|c| &c.ty).collect();
    let col_nullable: Vec<_> = columns.iter().map(|c| c.nullable).collect();
    let col_pk: Vec<_> = columns.iter().map(|c| c.pk).collect();
    let col_ai: Vec<_> = columns.iter().map(|c| c.auto_increment).collect();
    let col_idents: Vec<_> = columns.iter().map(|c| &c.ident).collect();

    let rel_variants: Vec<_> = relations.iter().map(|r| &r.variant).collect();
    let rel_idents: Vec<_> = relations.iter().map(|r| &r.ident).collect();
    let rel_names: Vec<_> = relations.iter().map(|r| r.name.as_str()).collect();
    let rel_kinds: Vec<_> = relations
        .iter()
        .map(|r| match r.kind {
            attr::RelationKind::HasMany => quote!(::ecat_orm::RelationKind::HasMany),
            attr::RelationKind::HasOne => quote!(::ecat_orm::RelationKind::HasOne),
            attr::RelationKind::BelongsTo => quote!(::ecat_orm::RelationKind::BelongsTo),
        })
        .collect();
    let rel_targets: Vec<_> = relations.iter().map(|r| &r.target).collect();
    let rel_fks: Vec<_> = relations.iter().map(|r| r.foreign_key.as_str()).collect();
    let rel_lks: Vec<_> = relations.iter().map(|r| &r.local_key).collect();
    let rel_defaults: Vec<_> = relations.iter().map(|r| &r.default).collect();
    // 写回口：多值全收，单值只取第一行。`rows` 为空时同样赋值 ——
    // 否则前一次加载残留在字段里的旧数据不会被清掉（静默给过时数据）。
    let rel_setters: Vec<_> = relations
        .iter()
        .map(|r| {
            let field = &r.ident;
            let target = &r.target;
            if r.kind == attr::RelationKind::HasMany {
                quote! {
                    let mut out = ::std::vec::Vec::with_capacity(rows.len());
                    for r in &rows {
                        out.push(<#target as ::ecat_orm::Entity>::from_row(r)?);
                    }
                    self.#field = out;
                }
            } else {
                quote! {
                    self.#field = match rows.first() {
                        ::core::option::Option::Some(r) => ::core::option::Option::Some(
                            <#target as ::ecat_orm::Entity>::from_row(r)?,
                        ),
                        ::core::option::Option::None => ::core::option::Option::None,
                    };
                }
            }
        })
        .collect();

    // 写回口本身。**没有关联时必须整段省掉**：`match name { _ => return Err(..) }`
    // 之后跟一个 `Ok(())`，编译器知道这个 match 必定发散，`Ok(())` 就成了
    // unreachable_code（`-D warnings` 直接红）；而只留一个通配 arm 的 match
    // 又会踩 clippy::match_single_binding。两条路都堵，只能分成两种函数体。
    let unknown_relation = quote! {
        ::ecat_orm::OrmError::UnknownColumn(
            ::std::format!("unknown relation `{name}`"),
        )
    };
    let set_relation = if relations.is_empty() {
        quote! {
            fn set_relation(
                &mut self,
                name: &str,
                _rows: ::std::vec::Vec<::ecat_orm::Row>,
            ) -> ::core::result::Result<(), ::ecat_orm::OrmError> {
                ::core::result::Result::Err(#unknown_relation)
            }
        }
    } else {
        quote! {
            fn set_relation(
                &mut self,
                name: &str,
                rows: ::std::vec::Vec<::ecat_orm::Row>,
            ) -> ::core::result::Result<(), ::ecat_orm::OrmError> {
                match name {
                    #(
                        #rel_names => {
                            #rel_setters
                        }
                    )*
                    _ => {
                        return ::core::result::Result::Err(#unknown_relation);
                    }
                }
                ::core::result::Result::Ok(())
            }
        }
    };

    // 关联枚举只在**有关联**时生成。空枚举没有任何可用变体，且它在测试等
    // 二进制目标里会因为「从未被使用」触发 dead_code。
    //
    // 必须放在 `const _` 块**外**：块内声明的类型在块外不可见，而用户要写的
    // 是 `Query::with(&[UserRelation::Posts])` —— 名字得能报出来。
    let rel_enum = (!relations.is_empty()).then(|| {
        let enum_ident = format_ident!("{}Relation", struct_ident);
        quote! {
            /// 由 `#[derive(Entity)]` 生成的关联选择器。
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub enum #enum_ident {
                #( #rel_variants, )*
            }

            impl #enum_ident {
                /// 关联名，与 `EntityMeta::relations` 里的 `name` 对应。
                pub fn name(self) -> &'static str {
                    match self {
                        #( Self::#rel_variants => #rel_names, )*
                    }
                }
            }

            impl ::ecat_orm::relation::RelationSelector for #enum_ident {
                fn name(self) -> &'static str {
                    self.name()
                }
            }
        }
    });

    let opt_str = |v: &Option<String>| match v {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    };
    let created_at = opt_str(&flags.created_at);
    let updated_at = opt_str(&flags.updated_at);
    let soft_delete = opt_str(&flags.soft_delete);
    let version = opt_str(&flags.version);

    Ok(quote! {
        // 匿名 const 块（serde 用的同款手法）：块内的 `impl` 照样被 rustc
        // 收集成全局 impl，但 `static META` 被关进块作用域。
        // 不套这一层的话，同一个模块里两个 `#[derive(Entity)]` 会同时声明
        // 模块级 `META`，直接 E0428 撞名 —— 而多实体同文件是最常见的写法。
        const _: () = {
            impl ::ecat_orm::Entity for #struct_ident {
                const TABLE: &'static str = #table;
                const PK: &'static str = #pk_name;
                const META: &'static ::ecat_orm::EntityMeta = &META;

                fn from_row(
                    row: &::ecat_orm::Row,
                ) -> ::core::result::Result<Self, ::ecat_orm::OrmError> {
                    ::core::result::Result::Ok(Self {
                        #(
                            #col_idents: ::ecat_orm::value::from_row_col::<#col_tys>(row, #col_names)?,
                        )*
                        // 结构体字面量必须列出**全部**字段：关联字段在这里
                        // 一律取「未预加载」值（空 Vec / None）。
                        #( #rel_idents: #rel_defaults, )*
                    })
                }

                fn to_values(
                    &self,
                ) -> ::std::vec::Vec<(&'static str, ::ecat_orm::serde_json::Value)> {
                    ::std::vec![
                        #(
                            (
                                #col_names,
                                ::ecat_orm::value::ColumnValue::to_json(&self.#col_idents),
                            ),
                        )*
                    ]
                }

                fn pk_value(&self) -> ::ecat_orm::serde_json::Value {
                    ::ecat_orm::value::ColumnValue::to_json(&self.#pk_ident)
                }

                #set_relation
            }

            // META 是本 impl 之外的一个 static：`impl` 里不能放 `static` 项，
            // 而 `META: &'static EntityMeta` 必须指向一个真正 'static 的值。
            #[doc(hidden)]
            #[allow(non_upper_case_globals)]
            static META: ::ecat_orm::EntityMeta = ::ecat_orm::EntityMeta {
                table: #table,
                pk: #pk_name,
                columns: &[
                    #(
                        ::ecat_orm::ColumnMeta {
                            name: #col_names,
                            // 列类型由 value.rs 的 ColumnValue impl 提供 —— 宏不复制
                            // 一份类型映射表，避免两处漂移。
                            ty: <#col_tys as ::ecat_orm::value::ColumnValue>::COL_TYPE,
                            nullable: #col_nullable,
                            pk: #col_pk,
                            auto_increment: #col_ai,
                        },
                    )*
                ],
                relations: &[
                    #(
                        ::ecat_orm::RelationMeta {
                            name: #rel_names,
                            kind: #rel_kinds,
                            target_table: <#rel_targets as ::ecat_orm::Entity>::TABLE,
                            foreign_key: #rel_fks,
                            local_key: #rel_lks,
                        },
                    )*
                ],
                flags: ::ecat_orm::EntityFlags {
                    created_at: #created_at,
                    updated_at: #updated_at,
                    soft_delete: #soft_delete,
                    version: #version,
                },
            };
        };

        #rel_enum
    })
}

#[derive(Default)]
struct FlagsAcc {
    created_at: Option<String>,
    updated_at: Option<String>,
    soft_delete: Option<String>,
    version: Option<String>,
}

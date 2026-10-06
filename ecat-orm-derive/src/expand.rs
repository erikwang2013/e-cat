// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use proc_macro2::TokenStream;
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
    let mut flags = FlagsAcc::default();
    let mut pk_ident: Option<syn::Ident> = None;
    let mut pk_name: Option<String> = None;

    for field in &named.named {
        let ident = field.ident.clone().expect("named field");
        let fa = attr::parse_field(&field.attrs)?;

        if let Some(rel) = &fa.relation {
            // Task 8 才实现。此处必须响亮报错：静默当作列的话，会去求
            // `Vec<Post>: ColumnValue`，用户看到的是一个和实际问题无关的
            // trait 未满足错误。
            return Err(syn::Error::new(
                field.span(),
                format!(
                    "`{}` is not implemented yet (batch-3 Task 8); \
                     remove it or implement relations first",
                    rel.kind.as_str()
                ),
            ));
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
                relations: &[],
                flags: ::ecat_orm::EntityFlags {
                    created_at: #created_at,
                    updated_at: #updated_at,
                    soft_delete: #soft_delete,
                    version: #version,
                },
            };
        };
    })
}

#[derive(Default)]
struct FlagsAcc {
    created_at: Option<String>,
    updated_at: Option<String>,
    soft_delete: Option<String>,
    version: Option<String>,
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 查询构建器。类型状态 `Unfiltered → Filtered` 把「无过滤条件的删改」
//! 变成编译错误 —— `User::query().delete_where(&db)` 不该能编译。

pub mod filter;
// `pub(crate)`：`crud::find_by_id` / `find_all` 要复用同一条 SELECT 生成路径
// （不另写一份 —— 写两份必然漂移）。
pub(crate) mod sql;
// `sql.rs` 的测试单独成文件：两者合在一起会顶过「每个源文件 < 500 行」的硬规则。
// 测试要够得着 `pub(crate)` 的生成函数，所以是 `query` 的子模块。
#[cfg(test)]
mod sql_tests;

use std::marker::PhantomData;

use serde_json::Value;

use crate::entity::Entity;
use crate::entity::EntityMeta;
use crate::error::OrmError;

pub use filter::Expr;
pub use filter::JoinType;
pub use filter::Op;
pub use filter::Order;
pub use filter::OrderBy;

/// 尚无过滤条件。
#[derive(Debug, Clone, Copy)]
pub struct Unfiltered;
/// 已有至少一个过滤条件 —— 只有这个状态能删改。
#[derive(Debug, Clone, Copy)]
pub struct Filtered;

/// 查询构建器。
///
/// **标识符白名单（spec §5.5a）**：`filter` / `order_by` 收的列名是 `&str`，
/// 一律对照 `EntityMeta.columns` 校验，未命中即返回 [`OrmError::UnknownColumn`]，
/// 绝不拼进 SQL。**白名单只覆盖主实体自己的列** —— `join()` 进来的关联表列不在
/// `EntityMeta.columns` 里（我们只有表名字符串，没有关联实体的元数据），
/// 要按关联表列过滤/排序得走 [`Query::filter_raw`]。这是有意的边界，不是遗漏。
#[derive(Debug)]
pub struct Query<E, S> {
    meta: &'static EntityMeta,
    filters: Vec<Expr>,
    orders: Vec<OrderBy>,
    /// `(连接类型, 表名, ON 条件)` —— 连接类型是**必须的**一维：
    /// 少了它，`LEFT JOIN` 会被渲染成裸 `JOIN`（= `INNER`），不报错、只给错结果。
    joins: Vec<(JoinType, String, String)>,
    limit: Option<u64>,
    offset: Option<u64>,
    with_trashed: bool,
    _marker: PhantomData<(fn() -> E, S)>,
}

impl<E: Entity> Default for Query<E, Unfiltered> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Entity> Query<E, Unfiltered> {
    /// 从空查询开始。`User::query()` 是它的语法糖。
    pub fn new() -> Self {
        Self::from_meta()
    }
}

impl<E: Entity, S> Query<E, S> {
    fn from_meta() -> Self {
        Self {
            meta: E::META,
            filters: Vec::new(),
            orders: Vec::new(),
            joins: Vec::new(),
            limit: None,
            offset: None,
            with_trashed: false,
            _marker: PhantomData,
        }
    }

    /// 关掉软删除的自动过滤。见「裁决 B」—— 在**两个状态上**都可用：
    /// 它是读取开关，与「有没有过滤条件」正交。
    pub fn with_trashed(mut self) -> Self {
        self.with_trashed = true;
        self
    }

    /// 排序。`column` 必须已在 `EntityMeta.columns` 里。
    pub fn order_by(mut self, column: &str, dir: Order) -> Result<Self, OrmError> {
        self.check_column(column)?;
        self.orders.push(OrderBy {
            column: column.into(),
            dir,
        });
        Ok(self)
    }

    /// 列名白名单校验 —— **本模块的安全核心**。
    fn check_column(&self, column: &str) -> Result<(), OrmError> {
        if self.meta.column(column).is_some() {
            Ok(())
        } else {
            Err(OrmError::UnknownColumn(column.into()))
        }
    }

    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// 加一个 JOIN。`table` 是**表名**，`on` 是原生 ON 条件。
    ///
    /// **两者都不经标识符白名单校验** —— 白名单只覆盖主实体自己的列
    /// （见 [`Query`] 的「标识符白名单」段）。`ON` 条件**必须由调用方保证可信**，
    /// 与 [`Query::filter_raw`] 同一信任边界。要按关联表的列过滤，也走 `filter_raw`。
    ///
    /// 按声明顺序渲染到 `FROM` 之后、`WHERE` 之前。
    pub fn join(mut self, kind: JoinType, table: &str, on: &str) -> Self {
        self.joins.push((kind, table.into(), on.into()));
        self
    }

    /// 关联表上的原生 WHERE 片段。**输入必须可信** —— 不校验、直接拼。
    ///
    /// **与 `filter` 一样把状态转成 [`Filtered`]**：类型状态的判据是
    /// 「**已经有一个过滤条件**」，而本方法正是加了一个。若它不转，
    /// 只用逃生口写条件的查询将**永远无法进入删改状态** ——
    /// 用户明明给了条件，却只能去加一个 dummy `.filter(...)` 才能调
    /// `delete_where`，那是没有安全收益的障碍。
    ///
    /// 逃生口的安全性由**调用方**负责（`filter_raw("1 = 1")` 确实会删全表，
    /// 但那是调用方显式写的），不由类型状态负责。
    pub fn filter_raw(self, expr: &str) -> Result<Query<E, Filtered>, OrmError> {
        Ok(self.push_filter(Expr::Raw(expr.into())))
    }

    // ---- 以下仅供本 crate 的测试与 SQL 生成使用 ----

    #[doc(hidden)]
    pub fn filter_count(&self) -> usize {
        self.filters.len()
    }
    #[doc(hidden)]
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }
    #[doc(hidden)]
    pub fn is_with_trashed(&self) -> bool {
        self.with_trashed
    }
}

impl<E: Entity, S> Query<E, S> {
    /// 加一个过滤条件。**返回新状态 `Filtered`** —— 这正是类型状态的作用。
    pub fn filter(
        self,
        column: &str,
        op: Op,
        value: impl Into<Value>,
    ) -> Result<Query<E, Filtered>, OrmError> {
        self.check_column(column)?;
        let value = value.into();
        let expr = match op {
            Op::IsNull => Expr::Null {
                column: column.into(),
                negated: false,
            },
            Op::NotNull => Expr::Null {
                column: column.into(),
                negated: true,
            },
            Op::In | Op::NotIn => {
                let arr = value.as_array().ok_or_else(|| {
                    OrmError::Rdbms(ecat_data::RdbmsError::Database(format!(
                        "`{}` requires an array value, got: {value}",
                        op.as_str()
                    )))
                })?;
                Expr::In {
                    column: column.into(),
                    values: arr.clone(),
                    negated: op == Op::NotIn,
                }
            }
            _ => Expr::Cmp {
                column: column.into(),
                op,
                value,
            },
        };
        Ok(self.push_filter(expr))
    }

    fn push_filter(mut self, expr: Expr) -> Query<E, Filtered> {
        self.filters.push(expr);
        Query {
            meta: self.meta,
            filters: self.filters,
            orders: self.orders,
            joins: self.joins,
            limit: self.limit,
            offset: self.offset,
            with_trashed: self.with_trashed,
            _marker: PhantomData,
        }
    }
}

/// 状态机契约的编译期断言。放在非 `#[cfg(test)]` 位置是**必需的** ——
/// `cargo test --doc` 编译 lib 时不开 `cfg(test)`，写在测试模块里的 doctest
/// 根本不存在；写在 `tests/*.rs` 里 rustdoc 也不收。两条都已实测复现。
///
/// 这里只放 doctest，没有可调用项，故整体 `#[doc(hidden)]`。
#[doc(hidden)]
pub mod _compile_fail_guards {
    /// 未过滤的 `Query` 造不出「已过滤」状态：`Filtered` 只能由
    /// [`Query::filter`] 产出。这是删改路径的类型基础 —— 一旦
    /// `Query<E, Filtered>` 能被凭空构造，`Filtered` 独占的那些方法
    /// （删除/更新入口）就失去了「必须有过滤条件」这道闸。
    ///
    /// ```compile_fail
    /// # use ecat_orm::query::{Filtered, Query};
    /// # fn f<E: ecat_orm::Entity>() {
    /// // 这行必须无法编译：`new()` 只存在于 `Query<E, Unfiltered>` 上
    /// let _: Query<E, Filtered> = Query::<E, Filtered>::new();
    /// # }
    /// ```
    ///
    /// 正向对照：**经过 `filter` 就能拿到 `Filtered`**。没有这一条，上面的
    /// `compile_fail` 可能「因为错误的原因通过」（任何编译错误都算通过，
    /// 包括方法名拼错）。
    ///
    /// ```no_run
    /// # use ecat_orm::query::{Filtered, Op, Query};
    /// # fn f<E: ecat_orm::Entity>() -> Result<(), ecat_orm::OrmError> {
    /// let _: Query<E, Filtered> = Query::new().filter("id", Op::Eq, 1)?;
    /// # Ok(()) }
    /// ```
    pub fn _guards() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::*;
    use serde_json::json;

    static COLS: [ColumnMeta; 3] = [
        ColumnMeta {
            name: "id",
            ty: ColType::I64,
            nullable: false,
            pk: true,
            auto_increment: true,
        },
        ColumnMeta {
            name: "name",
            ty: ColType::Text,
            nullable: false,
            pk: false,
            auto_increment: false,
        },
        ColumnMeta {
            name: "age",
            ty: ColType::I64,
            nullable: false,
            pk: false,
            auto_increment: false,
        },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users",
        pk: "id",
        columns: &COLS,
        relations: &[],
        flags: EntityFlags::NONE,
    };

    #[derive(Debug)]
    struct U;
    impl Entity for U {
        const TABLE: &'static str = "users";
        const PK: &'static str = "id";
        const META: &'static EntityMeta = &META;
        fn from_row(_r: &ecat_data::Row) -> Result<Self, OrmError> {
            Ok(U)
        }
        fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
            vec![]
        }
        fn pk_value(&self) -> serde_json::Value {
            json!(0)
        }
        fn set_relation(
            &mut self,
            _name: &str,
            _rows: Vec<ecat_data::Row>,
        ) -> Result<(), OrmError> {
            Ok(())
        }
    }

    // ---- 白名单 ----

    #[test]
    fn known_column_is_accepted() {
        assert!(U::query().filter("name", Op::Eq, "x").is_ok());
    }

    /// **安全约束**：未声明的列名必须被拒，且**不得出现在生成的 SQL 里**。
    #[test]
    fn unknown_column_is_rejected() {
        let e = U::query().filter("naem", Op::Eq, "x").unwrap_err();
        assert!(
            matches!(e, OrmError::UnknownColumn(ref c) if c == "naem"),
            "got: {e:?}"
        );
    }

    /// 注入尝试必须被白名单拦下 —— 它不在 columns 里。
    #[test]
    fn injection_attempt_is_rejected_as_unknown_column() {
        let e = U::query()
            .filter("name\" = 'x' OR 1=1 --", Op::Eq, "x")
            .unwrap_err();
        assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
    }

    #[test]
    fn unknown_order_by_column_is_rejected() {
        let e = U::query().order_by("nope", Order::Asc).unwrap_err();
        assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
    }

    // ---- 类型状态 ----

    #[test]
    fn filter_transitions_to_filtered() {
        let q: Query<U, Filtered> = U::query().filter("name", Op::Eq, "x").unwrap();
        assert_eq!(q.filter_count(), 1);
    }

    #[test]
    fn chaining_filters_accumulates() {
        let q = U::query()
            .filter("name", Op::Eq, "x")
            .unwrap()
            .filter("age", Op::Gt, 18)
            .unwrap();
        assert_eq!(q.filter_count(), 2);
    }

    // ---- 操作符 ----

    #[test]
    fn in_requires_an_array_value() {
        let ok = U::query().filter("id", Op::In, json!([1, 2, 3]));
        assert!(ok.is_ok());
        let bad = U::query().filter("id", Op::In, json!(1));
        assert!(
            matches!(bad, Err(OrmError::Rdbms(_))),
            "In 传非数组必须报错"
        );
    }

    #[test]
    fn is_null_ignores_the_value() {
        let q = U::query()
            .filter("name", Op::IsNull, json!("whatever"))
            .unwrap();
        assert_eq!(q.filter_count(), 1);
    }

    // ---- 裁决 B：with_trashed 在两个状态上都可用 ----

    #[test]
    fn with_trashed_works_before_and_after_filter() {
        let a = U::query().with_trashed().filter("id", Op::Eq, 1).unwrap();
        let b = U::query().filter("id", Op::Eq, 1).unwrap().with_trashed();
        assert!(a.is_with_trashed() && b.is_with_trashed());
    }

    // ---- 逃生口 ----

    #[test]
    fn filter_raw_bypasses_the_whitelist_by_design() {
        // 关联表的列走这里。文档必须写明「输入必须可信」。
        let q = U::query().filter_raw("posts.published = 1").unwrap();
        assert_eq!(q.filter_count(), 1);
    }

    /// **`filter_raw` 必须把状态转成 `Filtered`** —— 类型状态的判据是
    /// 「已经有一个过滤条件」，而它正是加了一个。
    ///
    /// 不转的后果：只用逃生口写条件的查询永远进不了删改状态，
    /// 用户得再加一个 dummy `.filter(..)` 才能调 `delete_where`。
    ///
    /// 本测试用**类型标注**钉住返回状态：`filter_raw` 若退回 `Self`，
    /// 这里会因为 `Query<U, Unfiltered>` 与 `Query<U, Filtered>` 不符而编译失败。
    #[test]
    fn filter_raw_transitions_to_filtered() {
        let q: Query<U, Filtered> = U::query().filter_raw("posts.published = 1").unwrap();
        assert_eq!(q.filter_count(), 1);
    }

    #[test]
    fn order_by_accepts_known_column_and_direction() {
        let q = U::query().order_by("id", Order::Desc).unwrap();
        assert_eq!(q.order_count(), 1);
    }
}

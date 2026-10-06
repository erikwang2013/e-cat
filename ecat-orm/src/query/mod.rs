// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 查询构建器。类型状态 `Unfiltered → Filtered` 把「无过滤条件的删改」
//! 变成编译错误 —— `User::query().delete_where(&db)` 不该能编译。

pub mod filter;
// `pub(crate)`：`crud::find_by_id` / `find_all` 要复用同一条 SELECT 生成路径
// （不另写一份 —— 写两份必然漂移）。
pub(crate) mod sql;
// 测试分成三个文件：合起来会顶过「每个源文件 < 500 行」的硬规则。
// 测试要够得着 `pub(crate)` 的生成函数，所以都是 `query` 的子模块。
#[cfg(test)]
mod builder_tests;
#[cfg(test)]
mod delete_tests;
#[cfg(test)]
mod sql_tests;

use std::marker::PhantomData;

use ecat_data::SqlExecutor;
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

impl<E: Entity, S> Query<E, S> {
    /// 执行查询，返回实体。
    ///
    /// 列清单、方言引号与占位符编号、软删除闸门、JOIN / ORDER BY / LIMIT
    /// 全由 [`sql::build_select`] 一处产出 —— `crud::find_all` 与
    /// `crud::find_by_id` 也走这里，**不另写 SQL 生成路径**（写两份必然漂移，
    /// 漂移的表现是「某些路径静默绕过软删除」）。
    pub async fn fetch<X>(&self, db: &X) -> Result<Vec<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        let built = sql::build_select(self, db.dialect(), false);
        let rows = db
            .query_with(&built.sql, &built.params)
            .await
            .map_err(OrmError::Rdbms)?;
        rows.iter().map(E::from_row).collect()
    }

    /// 复制一份并换掉分页参数（`paginate` 用）。
    ///
    /// **不能 `#[derive(Clone)]`**：那会给 `Query<E, S>` 加上 `E: Clone` 的要求，
    /// 而 `E` 只是类型状态里的 phantom。手写一遍字段就绕开了。
    pub(crate) fn clone_with_limit(&self, limit: u64, offset: u64) -> Self {
        Self {
            meta: self.meta,
            filters: self.filters.clone(),
            orders: self.orders.clone(),
            joins: self.joins.clone(),
            limit: Some(limit),
            offset: Some(offset),
            with_trashed: self.with_trashed,
            _marker: PhantomData,
        }
    }
}

impl<E: Entity> Query<E, Filtered> {
    /// 按当前过滤条件删除，**返回受影响行数**。
    ///
    /// - 实体声明了 `soft_delete` 标志时是**软删除**
    ///   （`UPDATE … SET <sd> = ? WHERE <过滤条件> AND <sd> IS NULL`），
    ///   与 `delete_by_id` 同语义（Task 14）。要物理删除用 [`Self::hard_delete_where`]。
    /// - **`with_trashed()` 在本路径上无效**：那是**读取**开关，
    ///   删改路径的「连已软删的一起处理」走的是 `hard_delete_where` 这个显式方法。
    /// - 过滤条件含 [`Op::In`] 时按 [`crate::dialect::DialectSpec::max_params_per_stmt`]
    ///   分块（spec:551），各块受影响行数**累加** —— 对 `IN` 语义等价。
    ///
    /// 实现是 [`crate::batch::delete_where`]：WHERE 渲染复用
    /// [`sql::render_where`]，本处只是把类型状态翻译成 `hard = false`。
    pub async fn delete_where<X>(self, db: &X) -> Result<u64, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        crate::batch::delete_where(db, self.meta, &self.filters, false).await
    }

    /// 绕过软删除，真的发 `DELETE`（连已软删的行一起送走）。
    /// 理由与 `hard_delete_by_id` 同：合规要求 / 垃圾回收确实需要物理删除，
    /// 不给口子会逼用户去拼裸 SQL。
    pub async fn hard_delete_where<X>(self, db: &X) -> Result<u64, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        crate::batch::delete_where(db, self.meta, &self.filters, true).await
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

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

use std::collections::HashMap;

use ecat_data::Row;
use ecat_data::SqlExecutor;
use serde_json::Value;

use crate::batch::chunk_size;
use crate::batch::split_chunks;
use crate::dialect::lookup;
use crate::entity::Entity;
use crate::entity::RelationKind;
use crate::error::OrmError;

/// 关联的方向。决定 `local_key` / `foreign_key` 哪一列属于谁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// 主体表（发起查询的那张）。
    Subject,
    /// 被查的目标表。
    Target,
}

/// 返回 `(取值的一侧, 匹配的一侧)`：
/// 从「取值的一侧」的列收集值，去「匹配的一侧」在**目标表**上的列里找。
///
/// - HasMany / HasOne：主体在 `local_key` 上有值，去目标的 `foreign_key` 里找
/// - BelongsTo：主体在 `foreign_key` 上有值，去目标的 `local_key` 里找
///
/// **写反了不会报错，只会查出一堆无关行。**
pub(crate) fn join_sides(kind: RelationKind) -> (Side, Side) {
    match kind {
        RelationKind::HasMany | RelationKind::HasOne => (Side::Subject, Side::Target),
        RelationKind::BelongsTo => (Side::Target, Side::Subject),
    }
}

/// 单值关联（取第一行）还是多值（全部）。
pub(crate) fn is_single_valued(kind: RelationKind) -> bool {
    matches!(kind, RelationKind::HasOne | RelationKind::BelongsTo)
}

/// 主体侧键值的去重集合（保持首次出现的顺序）。**跳过 NULL** ——
/// `IN (NULL)` 恒 UNKNOWN，查不出任何行，只是白跑一趟。
///
/// ponytail: `Vec::contains` 是 O(n²)，n 是单页主体数（分页上限内，几千顶天）。
/// 真出现几万主体的预加载再换 `HashSet` —— 那要多克隆一次每个键。
pub(crate) fn distinct_keys(keys: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for k in keys {
        if k.is_null() {
            continue;
        }
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// 按关联键分组。
pub(crate) fn group_by_key<K>(rows: Vec<(Value, K)>) -> HashMap<Value, Vec<K>> {
    let mut g: HashMap<Value, Vec<K>> = HashMap::new();
    for (k, v) in rows {
        g.entry(k).or_default().push(v);
    }
    g
}

/// 取主体某一列的值。主键走 `pk_value()`，其余列在 `to_values()` 里按列名找；
/// 找不到给 `Null`（该列不进 IN 列表）。
fn subject_value<E: Entity>(subject: &E, column: &str) -> Value {
    if column == E::PK {
        return subject.pk_value();
    }
    subject
        .to_values()
        .into_iter()
        .find(|(n, _)| *n == column)
        .map(|(_, v)| v)
        .unwrap_or(Value::Null)
}

/// 为一个关联发**一条**（键超过方言单语句参数上限时按 [`chunk_size`] 分块成少数几条）
/// `IN` 查询，把结果按主体分组写回。
///
/// 查询形态统一是 `SELECT * FROM <目标表> WHERE <目标列> IN (…)` —— HasMany /
/// HasOne / BelongsTo 只差「哪一列取值、哪一列匹配」（见 [`join_sides`]）。
/// 列清单用 `*` 是本设计唯一放宽列清单纪律的地方：目标表的列在编译期拿不到
/// （`RelationMeta` 只有表名），而 `from_row` 按列名取值，多出的列被忽略、
/// 缺列响亮报错 —— 安全性不依赖列清单。
///
/// 写回走 [`Entity::set_relation`]：`RelationMeta` 里只有表名，只有派生宏知道
/// 「`posts` 这个关联对应 `posts` 字段、目标类型是 `Post`」。
pub(crate) async fn load_relation<E, X>(
    db: &X,
    subjects: &mut [E],
    name: &str,
) -> Result<(), OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let meta = E::META
        .relation(name)
        .ok_or_else(|| OrmError::UnknownColumn(format!("unknown relation `{name}`")))?;

    // 「取值的一侧」出主体的列名，「匹配的一侧」出目标表的列名。
    // `Side::Subject` 取 `local_key`、`Side::Target` 取 `foreign_key`，
    // 两个方向都靠 `join_sides` 摆正（见它和 `join_sides_are_derived_per_kind`）。
    let (value_side, match_side) = join_sides(meta.kind);
    let subject_col = match value_side {
        Side::Subject => meta.local_key,
        Side::Target => meta.foreign_key,
    };
    let target_col = match match_side {
        Side::Subject => meta.local_key,
        Side::Target => meta.foreign_key,
    };

    let keys = distinct_keys(
        &subjects
            .iter()
            .map(|s| subject_value(s, subject_col))
            .collect::<Vec<_>>(),
    );

    let spec = lookup(db.dialect());
    let per_chunk = chunk_size(spec.max_params_per_stmt(), 1);

    // `keys` 为空（空主体列表，或键全是 NULL）时循环一次都不进 —— 不发语句。
    // **写回不在这里 return**：见下方注释。
    let mut buckets: HashMap<Value, Vec<Row>> = HashMap::new();
    let mut start = 0usize;
    for size in split_chunks(keys.len(), per_chunk) {
        let slice = &keys[start..start + size];
        start += size;

        let placeholders = (1..=slice.len())
            .map(|i| spec.placeholder(i))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT * FROM {} WHERE {} IN ({placeholders})",
            spec.quote(meta.target_table),
            spec.quote(target_col)
        );
        let rows = db.query_with(&sql, slice).await.map_err(OrmError::Rdbms)?;

        let mut pairs = Vec::with_capacity(rows.len());
        for r in rows {
            let k = r.get(target_col).ok_or_else(|| {
                OrmError::UnknownColumn(format!(
                    "relation `{name}`: target column `{target_col}` missing from the \
                     result set; the relation's foreign_key / local_key may be swapped"
                ))
            })?;
            pairs.push((k.clone(), r));
        }
        // 键在块之间不重复（`keys` 已去重），append 只是防「目标表返回了
        // 没查过的键」这一手：`extend` 会把已有的桶整个换掉，那是静默丢行。
        for (k, mut v) in group_by_key(pairs) {
            buckets.entry(k).or_default().append(&mut v);
        }
    }

    // 写回。**每个主体都调用** `set_relation` —— 即使它没有关联行（空 Vec）也必须调用：
    // 不调用的话，字段里残留的是上一次加载的旧数据（静默给过时数据）。
    //
    // **取用 `get` 而不是 `remove`**：主体键不唯一时（`local_key` 不是主键，
    // 或调用方给了重复行），同一个键的几个主体都该拿到那份关联行 ——
    // `remove` 只有第一个能拿到，其余静默变空。代价是每个主体克隆自己那一桶，
    // 总量就是「查回来的行数」，不多。
    //
    // 单值关联只交第一行：多给的会被 setter 丢掉，那不该由泛型层来假定。
    for s in subjects.iter_mut() {
        let mut rows = buckets
            .get(&subject_value(s, subject_col))
            .cloned()
            .unwrap_or_default();
        if is_single_valued(meta.kind) {
            rows.truncate(1);
        }
        s.set_relation(name, rows)?;
    }
    Ok(())
}

// 测试与夹具各成一文件：合进本文件会顶过「每个源文件 < 500 行」的硬规则。
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

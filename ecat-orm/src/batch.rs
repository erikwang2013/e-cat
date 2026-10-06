// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 批量操作：`insert_many` / `update_many` / `upsert` / `delete_where`。
//!
//! **每个批量口都按方言参数上限分块**（spec §5.5b）—— 各后端单语句参数上限
//! 差两个数量级（SQL Server 2100、SQLite 999、PG/MySQL 65535），不分块 =
//! 几千行的批量本来必然在真库上报错。
//!
//! 分块的两条不变量：
//! - 每块的行数 = `max_params_per_stmt / 每行参数数`，**向下取整且至少为 1**
//!   （向上取整就超限，取 0 就一行都不做 —— 静默丢数据，见 `chunk_size`）。
//! - 各块的受影响行数**累加**后返回。

use ecat_data::{RdbmsError, SqlExecutor};
use serde_json::Value;

use crate::crud::{insert_parts, update_row};
use crate::dialect::DialectSpec;
use crate::dialect::lookup;
use crate::entity::{Entity, EntityMeta};
use crate::error::OrmError;
use crate::query::Expr;
use crate::query::sql::{Placeholders, render_where};

// 测试分两个文件：合起来会顶过「每个源文件 < 500 行」的硬规则。
#[cfg(test)]
mod delete_tests;
#[cfg(test)]
mod tests;

fn to_db(e: RdbmsError) -> OrmError {
    OrmError::Rdbms(e)
}

/// 每块的行数 = 上限 / 每行参数数，**向下取整且至少为 1**。
///
/// 取 0 的后果是静默丢数据（`split_chunks` 一块都不产出），列数是用户定义的，
/// 不是常量 —— 所以「实际不会有 2100 列」不是删掉这条的理由。
pub(crate) fn chunk_size(max_params: usize, params_per_row: usize) -> usize {
    (max_params / params_per_row.max(1)).max(1)
}

/// 把 `total` 行切成每块最多 `per_chunk` 行的**尺寸**列表。
///
/// 返回尺寸而不是区间：调用方拿它直接切片，且 `Σ == total` 一眼可验
/// （漏掉的行不会报错，只会静默不写）。
pub(crate) fn split_chunks(total: usize, per_chunk: usize) -> Vec<usize> {
    let per_chunk = per_chunk.max(1);
    let mut out = Vec::new();
    let mut left = total;
    while left > 0 {
        let take = left.min(per_chunk);
        out.push(take);
        left -= take;
    }
    out
}

/// 批量插入，返回**受影响行数**（不是主键列表）。
///
/// 不回吐主键是 spec:504-506 的明确要求：MySQL 的 `LAST_INSERT_ID()` 只给批量的
/// 首行、SQLite 给末行，跨后端语义不可靠。要新主键请逐行 [`crate::crud::insert`]。
///
/// 自动时间戳**逐行**填（复用 [`insert_parts`]）—— 各写一份必然漂移，
/// 漂移的表现是「批量插入静默丢掉自动时间戳」。
pub(crate) async fn insert_many<E, X>(db: &X, entities: &[E]) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    if entities.is_empty() {
        return Ok(0); // 空输入不发语句 —— 生成 `VALUES ` 是语法错误
    }
    let spec = lookup(db.dialect());
    // 每行参数数 = 可插列数。用第一行推得，同一实体的各行列数一致。
    let (cols, _) = insert_parts(&entities[0]);
    if cols.is_empty() {
        return Err(to_db(RdbmsError::Database(
            "entity has no insertable columns".into(),
        )));
    }
    let per_chunk = chunk_size(spec.max_params_per_stmt(), cols.len());

    let mut total = 0u64;
    let mut start = 0usize;
    for size in split_chunks(entities.len(), per_chunk) {
        total += insert_chunk::<E, X>(db, &cols, &entities[start..start + size], spec).await?;
        start += size;
    }
    Ok(total)
}

/// 一条多行 `INSERT … VALUES (…), (…)`。
async fn insert_chunk<E, X>(
    db: &X,
    cols: &[String],
    rows: &[E],
    spec: &dyn DialectSpec,
) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let mut params: Vec<Value> = Vec::with_capacity(cols.len() * rows.len());
    let mut tuples = Vec::with_capacity(rows.len());
    let mut n = 0usize;
    for r in rows {
        let (row_cols, vals) = insert_parts(r);
        // 列集来自第一行；某行的列数与它不一致时，参数会**整体错位**
        // （静默写到别的列上），所以这里宁可报错。
        if row_cols.len() != cols.len() {
            return Err(to_db(RdbmsError::Database(
                "to_values() returned a different column count for a later row".into(),
            )));
        }
        let ph = vals
            .into_iter()
            .map(|v| {
                n += 1;
                params.push(v);
                spec.placeholder(n)
            })
            .collect::<Vec<_>>()
            .join(", ");
        tuples.push(format!("({ph})"));
    }
    let sql = format!(
        "INSERT INTO {} ({}) VALUES {}",
        spec.quote(E::TABLE),
        cols.iter()
            .map(|c| spec.quote(c))
            .collect::<Vec<_>>()
            .join(", "),
        tuples.join(", ")
    );
    db.execute_with(&sql, &params).await.map_err(to_db)
}

/// 按主键整行更新一批，返回**受影响行数之和**。
///
/// 每块的参数顺序是「主键、SET 的值、WHERE 的旧版本号」（见
/// [`crate::dialect::DialectSpec::update_many_stmt`]），分块只按行数切，
/// 所以顺序不会串。
///
/// **乐观锁在批量里报不出冲突**：单行 `update` 用「影响 0 行」判定冲突并返回
/// [`OrmError::OptimisticLockConflict`]，而批量只有总数 ——
/// **`返回行数 < 传入行数` 就是有人被版本闸门挡下了**（也可能是不够行不存在，
/// 两者在批量里不可分）。要精确知道是哪一行冲突，请逐行 [`crate::crud::update`]。
/// 逐行的版本条件照旧生效，不会静默覆盖别人的写。
pub(crate) async fn update_many<E, X>(db: &X, entities: &[E]) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    if entities.is_empty() {
        return Ok(0); // 空输入不发语句 —— `SET ` 后面什么都不剩是语法错误
    }
    let spec = lookup(db.dialect());
    let first = update_row(&entities[0]);
    if first.set_cols.is_empty() {
        return Err(to_db(RdbmsError::Database(
            "entity has no updatable columns".into(),
        )));
    }
    // 乐观锁：逐行比对旧版本号。版本列不在 SET 列里（`to_values` 没给）就没有
    // 这一维 —— 与单行 `update` 的判据一致。
    let where_extra: Vec<String> = match E::META.flags.version {
        Some(vc) if first.set_cols.iter().any(|c| c.as_str() == vc) => vec![vc.to_string()],
        _ => Vec::new(),
    };
    // 每行参数数 = 主键 + SET 列 + WHERE 额外列。**不能只用 SET 列数**：
    // 带 version 时每行多一个 WHERE 参数，少算会让分块超限。
    let per_row = 1 + first.set_cols.len() + where_extra.len();
    let per_chunk = chunk_size(spec.max_params_per_stmt(), per_row);

    let mut total = 0u64;
    let mut start = 0usize;
    for size in split_chunks(entities.len(), per_chunk) {
        total += update_chunk::<E, X>(
            db,
            &first.set_cols,
            &where_extra,
            &entities[start..start + size],
            spec,
        )
        .await?;
        start += size;
    }
    Ok(total)
}

/// 一条多行 `UPDATE`（方言各有形状，见 `DialectSpec::update_many_stmt`）。
async fn update_chunk<E, X>(
    db: &X,
    set_cols: &[String],
    where_extra: &[String],
    rows: &[E],
    spec: &dyn DialectSpec,
) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let mut params: Vec<Value> = Vec::with_capacity(rows.len() * (1 + set_cols.len() + 1));
    for r in rows {
        let row = update_row(r);
        if row.set_cols.len() != set_cols.len() {
            return Err(to_db(RdbmsError::Database(
                "to_values() returned a different column count for a later row".into(),
            )));
        }
        // 顺序必须与 `update_many_aliases` 一致：主键、SET 列、WHERE 额外列。
        params.push(r.pk_value());
        params.extend(row.set_values);
        for c in where_extra {
            let old = match E::META.flags.version {
                Some(vc) if vc == c => row.old_version.clone(),
                _ => None,
            };
            params.push(old.unwrap_or(Value::Null));
        }
    }
    let sql = spec.update_many_stmt(E::TABLE, E::PK, set_cols, where_extra, rows.len());
    db.execute_with(&sql, &params).await.map_err(to_db)
}

/// 单行 upsert（按主键）。方言各自用 `ON CONFLICT` / `ON DUPLICATE KEY` /
/// `MERGE`。要批量导入用 [`insert_many`] —— 它不处理主键冲突。
pub(crate) async fn upsert<E, X>(db: &X, e: &E) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let (cols, vals) = insert_parts(e);
    if cols.is_empty() {
        return Err(to_db(RdbmsError::Database(
            "entity has no insertable columns".into(),
        )));
    }
    let sql = spec.upsert(E::TABLE, &cols, E::PK, vals.len());
    db.execute_with(&sql, &vals).await.map_err(to_db)
}

/// 按过滤条件删除，返回**受影响行数之和**。
///
/// `hard == false` 且实体声明了 `soft_delete` 时是**软删除**
/// （`UPDATE … SET <sd> = ?`，与 `crud::delete_by_id` 同语义、同闸门）；
/// 其余情况是物理 `DELETE FROM`。
///
/// **WHERE 用的是 `query::sql::render_where`** —— 本模块**不重写**。
/// 两份 WHERE 渲染必然漂移，而那里是标识符白名单的安全边界。
///
/// `with_trashed()`（读取开关）**到不了这里**：调用方传的是 `hard`，
/// 不是 Query 上的标志 —— 这正是「删改路径不受读取开关影响」的落地方式。
///
/// `Op::In` 按 `max_params_per_stmt` 分块（spec:551），各块行数累加。
/// 上限只对 `In` 生效：非 In 条件本身堆到超限（几百个 AND 条件）不在此处处理。
///
/// ponytail: 分块只切 In；非 In 条件堆到超限是病态调用，不做通用切分。
pub(crate) async fn delete_where<X>(
    db: &X,
    meta: &'static EntityMeta,
    filters: &[Expr],
    hard: bool,
) -> Result<u64, OrmError>
where
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let soft = meta.flags.soft_delete.filter(|_| !hard);
    // 软删除只动还活着的行（闸门开着）；硬删除要连已软删的一起送走（闸门关掉）。
    let with_trashed = hard;
    // 软删除多一个 `SET <sd> = ?` 的参数，先把它从预算里扣掉。
    let budget = spec
        .max_params_per_stmt()
        .saturating_sub(usize::from(soft.is_some()));

    let mut total = 0u64;
    for chunk in split_in_filters(filters, budget) {
        let mut ph = Placeholders::new(spec);
        let mut params: Vec<Value> = Vec::new();
        let mut sql = match soft {
            Some(sd) => {
                let p = ph.take();
                params.push(crate::value::ColumnValue::to_json(&crate::crud::now()));
                format!(
                    "UPDATE {} SET {} = {p}",
                    spec.quote(meta.table),
                    spec.quote(sd)
                )
            }
            None => format!("DELETE FROM {}", spec.quote(meta.table)),
        };
        // 无条件可用时**报错而不是发一条无 WHERE 的语句**：那会删全表。
        // `Query<E, Filtered>` 的类型状态本该挡住这种调用，这里是第二道闸。
        let Some(w) = render_where(meta, &chunk, with_trashed, spec, &mut ph, &mut params) else {
            return Err(to_db(RdbmsError::Database(
                "delete_where requires at least one filter".into(),
            )));
        };
        sql.push_str(" WHERE ");
        sql.push_str(&w);

        debug_assert_eq!(ph.used(), params.len(), "占位符数与参数数必须相等");
        total += db.execute_with(&sql, &params).await.map_err(to_db)?;
    }
    Ok(total)
}

/// 非 `In` 条件占用的参数个数（`Cmp` 一个，`Null` / `Raw` 零个）。
fn fixed_param_cost(f: &Expr) -> usize {
    match f {
        Expr::Cmp { .. } => 1,
        Expr::In { .. } | Expr::Null { .. } | Expr::Raw(_) => 0,
    }
}

/// 把过滤条件切成若干组，每组做一条语句的参数总量不超 `budget`。
///
/// 非 In 条件在每条语句里都要**原样重复**（它们限定的是同一个集合），所以先从
/// 预算里扣掉；剩下的按 In 的个数平分，每个 In 的取值按份额切片；最后做
/// **笛卡尔积** —— 少一个组合就是少删一批数据，且不报错。
fn split_in_filters(filters: &[Expr], budget: usize) -> Vec<Vec<Expr>> {
    let ins: Vec<usize> = filters
        .iter()
        .enumerate()
        .filter(|(_, f)| matches!(f, Expr::In { .. }))
        .map(|(i, _)| i)
        .collect();
    if ins.is_empty() {
        return vec![filters.to_vec()];
    }
    let fixed: usize = filters.iter().map(fixed_param_cost).sum();
    let share = (budget.saturating_sub(fixed) / ins.len()).max(1);

    let mut out: Vec<Vec<Expr>> = vec![filters.to_vec()];
    for &idx in &ins {
        let Expr::In { values, .. } = &filters[idx] else {
            unreachable!("ins only contains In filters")
        };
        // 空列表给**一个空块**而不是零块：零块会让笛卡尔积为空 → 一条语句都不发，
        // 而 `IN ()` 的语义是恒假（`render_where` 渲染成 `1 = 0`），该发的还要发。
        let slices: Vec<Vec<Value>> = if values.is_empty() {
            vec![Vec::new()]
        } else {
            values.chunks(share).map(<[Value]>::to_vec).collect()
        };
        let mut next = Vec::with_capacity(out.len() * slices.len());
        for base in &out {
            for chunk in &slices {
                let mut f = base.clone();
                if let Expr::In { values, .. } = &mut f[idx] {
                    *values = chunk.clone();
                }
                next.push(f);
            }
        }
        out = next;
    }
    out
}

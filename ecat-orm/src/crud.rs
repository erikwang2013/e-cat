// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! CRUD。API 形态取自 spec §5.4 —— `User::insert(&db, &user)`，即全部是
//! `Entity` 的**关联函数**；`entity.rs` 里的默认方法只做转发，实现在这里。

use ecat_data::SqlExecutor;
use ecat_data::{RdbmsClient, RdbmsError, Row};
use serde_json::Value;

use crate::dialect::InsertPlan;
use crate::dialect::lookup;
use crate::entity::Entity;
use crate::error::OrmError;
use crate::query::Op;
use crate::query::Query;

/// 建 `INSERT` 的列清单与参数（**跳过自增主键** —— 它由数据库生成）。
fn insert_parts<E: Entity>(e: &E) -> (Vec<String>, Vec<Value>) {
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (name, v) in e.to_values() {
        let meta = E::META
            .column(name)
            .expect("to_values returned an unknown column");
        if meta.pk && meta.auto_increment {
            continue;
        }
        cols.push(name.to_string());
        vals.push(v);
    }
    (cols, vals)
}

/// 建 `UPDATE` 的 SET 片段（**跳过主键** —— 主键是定位条件，不是被更新的列）。
fn update_parts<E: Entity>(e: &E) -> (Vec<String>, Vec<Value>) {
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (name, v) in e.to_values() {
        let meta = E::META
            .column(name)
            .expect("to_values returned an unknown column");
        if meta.pk {
            continue;
        }
        cols.push(name.to_string());
        vals.push(v);
    }
    (cols, vals)
}

fn to_db(e: RdbmsError) -> OrmError {
    OrmError::Rdbms(e)
}

/// 从一行里取出主键回填值。
fn read_returned_pk<E: Entity>(rows: &[Row]) -> Result<i64, OrmError> {
    let row = rows.first().ok_or_else(|| {
        to_db(RdbmsError::Database(
            "INSERT returned no row for the generated primary key".into(),
        ))
    })?;
    crate::value::from_row_col::<i64>(row, E::PK)
}

/// 走查询构建器发一条 SELECT，返回原始行。
///
/// `find_by_id` / `find_all` 都经这里 —— **不另写一条 SQL 生成路径**：
/// 列清单、方言引号与占位符编号、软删除闸门全在 `Query` 一处。写两份必然漂移，
/// 而漂移的表现是「某些路径静默绕过了软删除」这种不报错的错。
/// （Task 15 的 `Query::fetch` 落地后，本函数可由它取代。）
async fn select_rows<E, X, S>(db: &X, q: &Query<E, S>) -> Result<Vec<Row>, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let built = crate::query::sql::build_select(q, db.dialect(), false);
    db.query_with(&built.sql, &built.params)
        .await
        .map_err(to_db)
}

/// 插入并返回新生成的主键。
///
/// **取 `RdbmsClient` 而不是 `SqlExecutor`**：MySQL 走两步式主键回填，
/// 必须能把两条语句包进同一个事务（见下），而 `SqlExecutor` 上没有任何
/// 「开事务 / 拿到固定连接」的入口 —— `transaction()` 在 `RdbmsClient`
/// 上（`ecat-data/src/rdbms.rs:213`）。照计划写 `X: SqlExecutor` 会得到
/// `E0599: no method named 'transaction' found for reference '&X'`。
pub(crate) async fn insert<E, X>(db: &X, e: &E) -> Result<i64, OrmError>
where
    E: Entity,
    X: RdbmsClient + ?Sized,
{
    let spec = lookup(db.dialect());
    let (cols, vals) = insert_parts(e);
    let plan = spec.insert_plan(E::TABLE, &cols, E::PK, vals.len());

    match plan {
        InsertPlan::Single { sql } => {
            // 一步式：RETURNING / OUTPUT INSERTED 走 query_write —— 写路径需要
            // 返回结果，读写分离路由必须把它发到主库（`query_write` 的存在理由）。
            let rows = db.query_write(&sql, &vals).await.map_err(to_db)?;
            read_returned_pk::<E>(&rows)
        }
        InsertPlan::InsertThen { insert, fetch } => {
            // **两步式必须包事务**：`LAST_INSERT_ID()` 是连接作用域的，
            // 池下两次 query_write 可能落到不同连接，取回别的会话刚插入的 id
            // （静默错值）。见 spec:585-589。
            let tx = db.transaction().await.map_err(to_db)?;
            tx.execute_with(&insert, &vals).await.map_err(to_db)?;
            let rows = tx.query(&fetch).await.map_err(to_db)?;
            let id = read_returned_pk::<E>(&rows)?;
            tx.commit().await.map_err(to_db)?;
            Ok(id)
        }
    }
}

/// 按主键取一行。列清单与软删除闸门交给查询构建器。
pub(crate) async fn find_by_id<E, X, K>(db: &X, pk: K) -> Result<Option<E>, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let q = E::query().filter(E::PK, Op::Eq, pk)?;
    select_rows(db, &q)
        .await?
        .first()
        .map(E::from_row)
        .transpose()
}

/// 取该实体的全部行（不带任何过滤）。等价于 `Self::query().fetch(db)`。
pub(crate) async fn find_all<E, X>(db: &X) -> Result<Vec<E>, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let rows = select_rows(db, &E::query()).await?;
    rows.iter().map(E::from_row).collect()
}

/// 按主键整行更新。影响 0 行返回 [`OrmError::NotFound`]。
pub(crate) async fn update<E, X>(db: &X, e: &E) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let (cols, mut vals) = update_parts(e);
    let set = cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} = {}", spec.quote(c), spec.placeholder(i + 1)))
        .collect::<Vec<_>>()
        .join(", ");
    let pk_ph = spec.placeholder(vals.len() + 1);
    let sql = format!(
        "UPDATE {} SET {set} WHERE {} = {pk_ph}",
        spec.quote(E::TABLE),
        spec.quote(E::PK)
    );
    vals.push(e.pk_value());
    let n = db.execute_with(&sql, &vals).await.map_err(to_db)?;
    if n == 0 {
        return Err(OrmError::NotFound);
    }
    Ok(n)
}

/// 按主键删除。影响 0 行返回 [`OrmError::NotFound`]。
pub(crate) async fn delete_by_id<E, X, K>(db: &X, pk: K) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let spec = lookup(db.dialect());
    let sql = format!(
        "DELETE FROM {} WHERE {} = {}",
        spec.quote(E::TABLE),
        spec.quote(E::PK),
        spec.placeholder(1)
    );
    let n = db.execute_with(&sql, &[pk.into()]).await.map_err(to_db)?;
    if n == 0 {
        return Err(OrmError::NotFound);
    }
    Ok(n)
}

/// 主键为「未设置」时插入，否则更新。返回主键。
pub(crate) async fn save<E, X>(db: &X, e: &E) -> Result<i64, OrmError>
where
    E: Entity,
    X: RdbmsClient + ?Sized,
{
    let pk_meta = E::META.column(E::PK).expect("PK must be a declared column");
    // 「未设置」= **自增**主键为 0。少了 auto_increment 这一半，
    // 手工分配整数主键的实体每次 save 都会变成 insert（主键冲突或重复行）。
    // 用 `as_i64()` 而不是 `Value::from(0)` 比较：后者要给每次判断造一个
    // owned `Value`（clippy::cmp_owned），且 JSON 的 `0` 与 `0.0` 是否相等
    // 取决于 serde_json 的数值归一化 —— 显式要求「整数 0」没有歧义。
    let unset = pk_meta.auto_increment && e.pk_value().as_i64() == Some(0);
    if unset {
        insert(db, e).await
    } else {
        update(db, e).await?;
        // update 成功时主键不变，原样返回，让 save 两条路径的返回类型一致。
        e.pk_value().as_i64().ok_or_else(|| {
            to_db(RdbmsError::Database(
                "primary key is not an integer; save() returns i64".into(),
            ))
        })
    }
}

// 夹具与断言分两个文件：合在一起会顶过「每个源文件 < 500 行」的硬规则。
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

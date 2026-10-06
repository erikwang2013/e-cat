// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! CRUD。API 形态取自 spec §5.4 —— `User::insert(&db, &user)`，即全部是
//! `Entity` 的**关联函数**；`entity.rs` 里的默认方法只做转发，实现在这里。

use ecat_data::{RdbmsError, Row, SqlExecutor};
use serde_json::Value;

use crate::dialect::InsertPlan;
use crate::dialect::lookup;
use crate::entity::Entity;
use crate::error::OrmError;
use crate::query::Op;

/// 当前时间。
///
/// **不做可注入的全局时钟** —— 那是个进程级 `static`，一个测试设了固定时间会污染
/// 同二进制的其它测试（`cargo test` 默认并行跑，行为随机）。批次 1 在 clickhouse 的
/// TTL 测试上吃过同族问题。可测性由**断言的选择**提供：见 `crud/auto_tests.rs` 用
/// `assert_ne!` 与「与此刻相差 < 1 分钟」这类性质断言，都不依赖具体时刻。
/// `pub(crate)`：`batch::delete_where` 的软删除路径也要填删除时刻。
pub(crate) fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

/// 按 `flags` 填入自动时间戳，返回该列的最终值（`None` = 保持原值）。
///
/// `always_overwrite` 为真时无条件覆盖；为假时**只在当前值为 NULL 时**填。
/// 这个不对称是**故意的**（spec:522-523）：
/// - `created_at`（假）：事实记录 —— 数据导入时要保留原始创建时间，显式值优先
/// - `updated_at`（真）：变更追踪 —— insert 与 update 都填，显式传旧值没有意义
///
/// **不要为了「看着匀称」把它改成一致**：静默改掉会让数据导入场景丢时间。
fn auto_timestamp(
    flags_value: Option<&'static str>,
    column: &str,
    current: &Value,
    always_overwrite: bool,
) -> Option<Value> {
    if flags_value != Some(column) {
        return None;
    }
    if always_overwrite || current.is_null() {
        Some(crate::value::ColumnValue::to_json(&now()))
    } else {
        None
    }
}

/// 建 `INSERT` 的列清单与参数（**跳过自增主键** —— 它由数据库生成）。
///
/// 时间戳在**构造列清单时**填，不修改实体本身：`insert` 取 `&E`，没有可变性可用，
/// 也**不该**有 —— 让 `insert` 偷偷改动调用方的实体是意外副作用。
///
/// `pub(crate)`：`batch::insert_many` / `batch::upsert` 复用同一份 ——
/// 各写一份必然漂移，而漂移的表现是「批量插入静默丢掉自动时间戳」。
pub(crate) fn insert_parts<E: Entity>(e: &E) -> (Vec<String>, Vec<Value>) {
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (name, mut v) in e.to_values() {
        let meta = E::META
            .column(name)
            .expect("to_values returned an unknown column");
        if meta.pk && meta.auto_increment {
            continue;
        }
        if let Some(filled) = auto_timestamp(E::META.flags.created_at, name, &v, false) {
            v = filled;
        }
        if let Some(filled) = auto_timestamp(E::META.flags.updated_at, name, &v, true) {
            v = filled;
        }
        cols.push(name.to_string());
        vals.push(v);
    }
    (cols, vals)
}

/// 建 `UPDATE` 的 SET 片段（**跳过主键** —— 主键是定位条件，不是被更新的列）。
///
/// 只刷新 `updated_at`：`created_at` 是事实记录，update 不得动它。
fn update_parts<E: Entity>(e: &E) -> (Vec<String>, Vec<Value>) {
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (name, mut v) in e.to_values() {
        let meta = E::META
            .column(name)
            .expect("to_values returned an unknown column");
        if meta.pk {
            continue;
        }
        if let Some(filled) = auto_timestamp(E::META.flags.updated_at, name, &v, true) {
            v = filled;
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

/// 插入并返回新生成的主键。
///
/// 取 `SqlExecutor`（而不是 `RdbmsClient`）：MySQL 的两步式主键回填经
/// [`SqlExecutor::execute_then_query`] 发出，**客户端与 `Transaction` 都收** ——
/// - 传客户端 → 它开一个事务把两条语句包起来（`LAST_INSERT_ID()` 是连接
///   作用域的，池下两条各取连接可能落到不同连接、取回别的会话的 id：静默错值）
/// - 传 `Transaction` → 直接在**调用方已有的事务**里跑，不另开也不提交，
///   原子边界归调用方（spec §5.4 的 `User::insert(&tx, &user)`）
pub(crate) async fn insert<E, X>(db: &X, e: &E) -> Result<i64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
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
            // **两步式必须原子**（见函数文档）：`LAST_INSERT_ID()` 是连接作用域的，
            // 池下直发两条可能落到不同连接，取回别的会话刚插入的 id（静默错值，
            // 见 spec:585-589）。交给 executor：客户端自己开事务包两条，
            // `Transaction` 直接复用调用方的。
            let rows = db
                .execute_then_query(&insert, &vals, &fetch)
                .await
                .map_err(to_db)?;
            read_returned_pk::<E>(&rows)
        }
    }
}

/// 按主键取一行。列清单与软删除闸门交给查询构建器。
///
/// 走 [`crate::query::Query::fetch`] —— **不另写一条 SQL 生成路径**：
/// 列清单、方言引号与占位符编号、软删除闸门全在 `Query` 一处。写两份必然漂移，
/// 而漂移的表现是「某些路径静默绕过了软删除」这种不报错的错。
pub(crate) async fn find_by_id<E, X, K>(db: &X, pk: K) -> Result<Option<E>, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let q = E::query().filter(E::PK, Op::Eq, pk)?;
    Ok(q.fetch(db).await?.into_iter().next())
}

/// 取该实体的全部行（不带任何过滤）。等价于 `Self::query().fetch(db)`。
pub(crate) async fn find_all<E, X>(db: &X) -> Result<Vec<E>, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    E::query().fetch(db).await
}

/// 单行 UPDATE 的 SET 列/参数与乐观锁要用的**旧**版本号。
///
/// `crud::update`（单行）与 `batch::update_many`（多行）**共用同一份** ——
/// 两份必然漂移，漂移的表现是「批量路径静默漏掉版本闸门」这种不报错的错。
pub(crate) struct UpdateRow {
    /// SET 的目标列（不含主键），顺序与 `set_values` 一一对应。
    pub(crate) set_cols: Vec<String>,
    /// SET 的参数。带 `version` flag 时，version 那一格已经是**新值**（旧值 + 1）。
    pub(crate) set_values: Vec<Value>,
    /// 乐观锁：WHERE 要比对的旧版本号（`None` = 该实体没有 version flag）。
    pub(crate) old_version: Option<Value>,
}

/// 按 `update` 的规则生成一行的 SET 部件（含乐观锁的「SET 写新值」半边）。
pub(crate) fn update_row<E: Entity>(e: &E) -> UpdateRow {
    let (set_cols, mut set_values) = update_parts(e);

    // 乐观锁：SET 写**新值**（旧值 + 1）、WHERE 比对**旧值** —— 于是「读-改-写」
    // 之间的并发改动会让 UPDATE 影响 0 行，而不是静默覆盖别人刚写的值。
    // 旧值必须在覆盖前取走：覆盖之后 `vals[pos]` 里只剩新值。
    let mut old_version = None;
    if let Some(vc) = E::META.flags.version
        && let Some(pos) = set_cols.iter().position(|c| c.as_str() == vc)
    {
        let old = set_values[pos].as_i64().unwrap_or(0);
        old_version = Some(set_values[pos].clone());
        set_values[pos] = Value::from(old + 1);
    }

    UpdateRow {
        set_cols,
        set_values,
        old_version,
    }
}

/// 按主键整行更新。
///
/// - 影响 0 行且实体声明了 `version` flag → [`OrmError::OptimisticLockConflict`]
/// - 影响 0 行且无 `version` → [`OrmError::NotFound`]
///
/// 两者**必须可区分**：前者重试有意义（重新加载再试），后者没有。
pub(crate) async fn update<E, X>(db: &X, e: &E) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let row = update_row(e);
    let set = row
        .set_cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} = {}", spec.quote(c), spec.placeholder(i + 1)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut vals = row.set_values;

    let mut where_parts = vec![format!(
        "{} = {}",
        spec.quote(E::PK),
        spec.placeholder(vals.len() + 1)
    )];
    vals.push(e.pk_value());
    if let (Some(vc), Some(old)) = (E::META.flags.version, row.old_version) {
        where_parts.push(format!(
            "{} = {}",
            spec.quote(vc),
            spec.placeholder(vals.len() + 1)
        ));
        vals.push(old);
    }

    let sql = format!(
        "UPDATE {} SET {set} WHERE {}",
        spec.quote(E::TABLE),
        where_parts.join(" AND ")
    );

    let n = db.execute_with(&sql, &vals).await.map_err(to_db)?;
    if n == 0 {
        // 带 version 的实体：0 行几乎总是版本不匹配（行被别人改过），
        // 而不是行不存在 —— 报冲突让调用方知道该重新加载再试。
        return Err(if E::META.flags.version.is_some() {
            OrmError::OptimisticLockConflict
        } else {
            OrmError::NotFound
        });
    }
    Ok(n)
}

/// 按主键删除。影响 0 行返回 [`OrmError::NotFound`]。
///
/// **软删除实体（`flags.soft_delete`）发的是 `UPDATE`，不是 `DELETE`** ——
/// 真的要物理删除见 [`hard_delete_by_id`]。
pub(crate) async fn delete_by_id<E, X, K>(db: &X, pk: K) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let spec = lookup(db.dialect());
    let pk: Value = pk.into();

    // 一条 match 同时产出 SQL 与参数：分两次判断同一个 flag，改了一处漏另一处
    // 就是「参数与语句错位」—— 静默错值，不报错。
    let (sql, params) = match E::META.flags.soft_delete {
        Some(sd) => (
            // `AND sd IS NULL`：重复删除同一行影响 0 行 → NotFound，
            // 而不是把 deleted_at 覆盖成新时刻（那会让「何时删的」失真）。
            format!(
                "UPDATE {} SET {} = {} WHERE {} = {} AND {} IS NULL",
                spec.quote(E::TABLE),
                spec.quote(sd),
                spec.placeholder(1),
                spec.quote(E::PK),
                spec.placeholder(2),
                spec.quote(sd)
            ),
            vec![crate::value::ColumnValue::to_json(&now()), pk],
        ),
        None => (
            format!(
                "DELETE FROM {} WHERE {} = {}",
                spec.quote(E::TABLE),
                spec.quote(E::PK),
                spec.placeholder(1)
            ),
            vec![pk],
        ),
    };

    let n = db.execute_with(&sql, &params).await.map_err(to_db)?;
    if n == 0 {
        return Err(OrmError::NotFound);
    }
    Ok(n)
}

/// 绕过软删除，真的发 `DELETE`。软删除实体有时确实需要物理删除
/// （合规要求、垃圾回收）—— 不提供这个口子会逼用户去拼裸 SQL。
pub(crate) async fn hard_delete_by_id<E, X, K>(db: &X, pk: K) -> Result<u64, OrmError>
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

/// 主键为「未设置」时插入，否则更新。**返回 `()`，不回吐主键。**
///
/// 需要新生成的主键时用 [`insert`]（自增主键必然是整数）；更新路径的主键调用方
/// 本来就有（在实体上）。旧版返回 `i64` 时，字符串主键（UUID）实体会在
/// **UPDATE 已经成功之后**才报错 —— 数据写进去了，调用方拿到 `Err`。
///
/// 两条路径都只要求 `SqlExecutor`，所以「事务里 save」与「事务里 insert」
/// 一样成立（插入路径的两步式见 [`insert`]）。
pub(crate) async fn save<E, X>(db: &X, e: &E) -> Result<(), OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let pk_meta = E::META.column(E::PK).expect("PK must be a declared column");
    // 「未设置」= **自增**主键为 0。少了 auto_increment 这一半，
    // 手工分配整数主键的实体每次 save 都会变成 insert（主键冲突或重复行）。
    // 用 `as_i64()` 而不是 `Value::from(0)` 比较：后者要给每次判断造一个
    // owned `Value`（clippy::cmp_owned），且 JSON 的 `0` 与 `0.0` 是否相等
    // 取决于 serde_json 的数值归一化 —— 显式要求「整数 0」没有歧义。
    let unset = pk_meta.auto_increment && e.pk_value().as_i64() == Some(0);
    if unset {
        insert(db, e).await.map(|_| ())
    } else {
        update(db, e).await.map(|_| ())
    }
}

// 夹具与断言分三个文件：合在一起会顶过「每个源文件 < 500 行」的硬规则。
// `pub(crate)`：`batch` 的测试也复用这两份夹具 —— 各写一份必然漂移，
// 漂移的表现是「批量路径的自动行为与单行路径不一致」。
#[cfg(test)]
pub(crate) mod auto_tests;
#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;

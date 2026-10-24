// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 跨边界查询：按 `hot_days` 把区间拆到热/冷两侧，按主键合并去重后分页。
use std::cmp::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, Row};
use serde_json::Value;
use time::macros::format_description;
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::{TieringError, validate_ident};

/// 冷层区间查询抽象；实现方（如 ClickHouse 适配）自己绑定参数。
#[async_trait]
pub trait ColdQuery: Send + Sync {
    /// 返回 `[from, to]` 内的行。`params` 原样下发给冷层：热层 SQL 只由表元数据
    /// 与区间构造，不接收任意谓词。
    async fn query_range(
        &self,
        table: &str,
        from: OffsetDateTime,
        to: OffsetDateTime,
        params: &[Value],
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Row>, TieringError>;
}

/// 单表的分层查询元数据。
pub struct TieredTable {
    pub name: String,
    pub time_col: String,
    pub id_col: String,
    pub hot_days: u32,
}

/// 跨热/冷边界的有界区间查询。
pub struct TieredQuery {
    hot: Arc<dyn RdbmsClient>,
    cold: Arc<dyn ColdQuery>,
    tables: Vec<TieredTable>,
}

impl TieredQuery {
    pub fn new(
        hot: Arc<dyn RdbmsClient>,
        cold: Arc<dyn ColdQuery>,
        tables: Vec<TieredTable>,
    ) -> Self {
        Self { hot, cold, tables }
    }

    /// 查询 `[from, to]` 内最多 `limit` 行（跳过 `offset`）。
    ///
    /// 区间全在热层保留期内只查热层，全在边界外只查冷层；跨边界时两侧各取
    /// `limit + offset` 行，按主键合并去重、排序后再分页。冷层失败直接返回错误。
    pub async fn query_range(
        &self,
        table: &str,
        from: OffsetDateTime,
        to: OffsetDateTime,
        params: &[Value],
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Row>, TieringError> {
        let dialect = self.hot.dialect();
        if dialect == Dialect::Mssql {
            return Err(TieringError::UnsupportedDialect);
        }
        let table = self
            .tables
            .iter()
            .find(|t| t.name == table)
            .ok_or_else(|| TieringError::Config(format!("unknown tiered table `{table}`")))?;
        validate_ident(&table.name)?;
        validate_ident(&table.time_col)?;
        validate_ident(&table.id_col)?;

        let boundary = OffsetDateTime::now_utc() - Duration::days(i64::from(table.hot_days));
        if from >= boundary {
            return self
                .hot_range(table, dialect, from, to, limit, offset)
                .await;
        }
        if to < boundary {
            return self
                .cold
                .query_range(&table.name, from, to, params, limit, offset)
                .await;
        }
        // 跨边界：任一侧都可能贡献前 limit+offset 行，两侧都按它下推。
        let pushdown = limit.saturating_add(offset);
        let mut rows = self
            .hot_range(table, dialect, boundary, to, pushdown, 0)
            .await?;
        rows.extend(
            self.cold
                .query_range(&table.name, from, boundary, params, pushdown, 0)
                .await?,
        );
        sort_dedupe_paginate(table, rows, limit, offset)
    }

    async fn hot_range(
        &self,
        table: &TieredTable,
        dialect: Dialect,
        from: OffsetDateTime,
        to: OffsetDateTime,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Row>, TieringError> {
        let sql = format!(
            "SELECT * FROM {table} WHERE {ts} >= {p1} AND {ts} <= {p2} \
             ORDER BY {id} LIMIT {p3} OFFSET {p4}",
            table = quote_ident(dialect, &table.name),
            ts = quote_ident(dialect, &table.time_col),
            id = quote_ident(dialect, &table.id_col),
            p1 = placeholder(dialect, 1),
            p2 = placeholder(dialect, 2),
            p3 = placeholder(dialect, 3),
            p4 = placeholder(dialect, 4),
        );
        let params = [
            Value::String(fmt_time(from)?),
            Value::String(fmt_time(to)?),
            Value::Number(as_i64(limit, "limit")?.into()),
            Value::Number(as_i64(offset, "offset")?.into()),
        ];
        Ok(self.hot.query_with(&sql, &params).await?)
    }
}

/// 合并后按主键排序、去重（相邻同键，稳定序保留热层副本），再切页。
fn sort_dedupe_paginate(
    table: &TieredTable,
    rows: Vec<Row>,
    limit: u64,
    offset: u64,
) -> Result<Vec<Row>, TieringError> {
    let mut keyed = rows
        .into_iter()
        .map(|row| {
            let key = row.get(&table.id_col).cloned().ok_or_else(|| {
                TieringError::State(format!("row has no `{}` column", table.id_col))
            })?;
            Ok((key, row))
        })
        .collect::<Result<Vec<_>, TieringError>>()?;
    keyed.sort_by(|(a, _), (b, _)| cmp_pk(a, b));
    keyed.dedup_by(|(a, _), (b, _)| cmp_pk(a, b) == Ordering::Equal);
    let skip = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    Ok(keyed
        .into_iter()
        .skip(skip)
        .take(take)
        .map(|(_, row)| row)
        .collect())
}

/// 主键全序：先按类型分层，同层内数字按数值、字符串按字典序。
fn cmp_pk(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => match (a.as_i64(), b.as_i64()) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => a
                .as_f64()
                .partial_cmp(&b.as_f64())
                .unwrap_or(Ordering::Equal),
        },
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => type_rank(a)
            .cmp(&type_rank(b))
            .then_with(|| a.to_string().cmp(&b.to_string())),
    }
}

fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Number(_) => 0,
        Value::String(_) => 1,
        Value::Bool(_) => 2,
        Value::Null => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn quote_ident(dialect: Dialect, ident: &str) -> String {
    if dialect == Dialect::MySql {
        format!("`{ident}`")
    } else {
        format!("\"{ident}\"")
    }
}

fn placeholder(dialect: Dialect, n: usize) -> String {
    match dialect {
        Dialect::Postgres => format!("${n}"),
        _ => "?".to_string(),
    }
}

fn as_i64(n: u64, what: &str) -> Result<i64, TieringError> {
    i64::try_from(n).map_err(|_| TieringError::Config(format!("{what} out of range: {n}")))
}

fn fmt_time(ts: OffsetDateTime) -> Result<String, TieringError> {
    ts.to_offset(UtcOffset::UTC)
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second]"
        ))
        .map_err(|e| TieringError::State(format!("failed to format boundary: {e}")))
}

#[cfg(test)]
mod tests;

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! ClickHouse 冷层 `ColdQuery` 适配器。
//!
//! `ClickhouseClient` 没有参数绑定，所有标识符过 `validate_ident`，时间值经
//! `render_sql_value` 转义；SQL 结构固定，不接受任意谓词。
use async_trait::async_trait;
use ecat_data::Row;
use ecat_data::SqlExecutor;
use ecat_data_clickhouse::ClickhouseClient;
use serde_json::Value;
use time::OffsetDateTime;

use crate::{ColdQuery, TieringError, render_sql_value, validate_ident};

use super::fmt_time;

pub struct ClickhouseColdQuery {
    client: ClickhouseClient,
    table: String,
    time_col: String,
    id_col: String,
}

impl ClickhouseColdQuery {
    pub fn new(
        client: ClickhouseClient,
        table: String,
        time_col: String,
        id_col: String,
    ) -> Result<Self, TieringError> {
        for ident in [&table, &time_col, &id_col] {
            validate_ident(ident)?;
        }
        Ok(Self {
            client,
            table,
            time_col,
            id_col,
        })
    }
}

#[async_trait]
impl ColdQuery for ClickhouseColdQuery {
    async fn query_range(
        &self,
        table: &str,
        from: OffsetDateTime,
        to: OffsetDateTime,
        params: &[Value],
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Row>, TieringError> {
        if table != self.table {
            return Err(TieringError::Config(format!(
                "ClickHouse cold query is bound to table `{}`, got `{table}`",
                self.table
            )));
        }
        if !params.is_empty() {
            return Err(TieringError::Config(
                "ClickHouse cold query does not support bound parameters".into(),
            ));
        }
        let from = fmt_time(from)?;
        let to = fmt_time(to)?;
        let sql = build_range_sql(
            &self.table,
            &self.time_col,
            &self.id_col,
            &from,
            &to,
            limit,
            offset,
        )?;
        Ok(self.client.query(&sql).await?)
    }
}

/// 固定 SQL 形状：PK 升序在 LIMIT/OFFSET 之前。
pub fn build_range_sql(
    table: &str,
    time_col: &str,
    id_col: &str,
    from: &str,
    to: &str,
    limit: u64,
    offset: u64,
) -> Result<String, TieringError> {
    for ident in [table, time_col, id_col] {
        validate_ident(ident)?;
    }
    let from = render_sql_value(&Value::String(from.to_owned()))?;
    let to = render_sql_value(&Value::String(to.to_owned()))?;
    Ok(format!(
        "SELECT * FROM {table} WHERE {time_col} >= {from} AND {time_col} < {to} \
         ORDER BY {id_col} ASC LIMIT {limit} OFFSET {offset}"
    ))
}

#[cfg(test)]
mod tests;

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 显式 scatter：同一条 SQL 扇出到表的所有分片，归并排序/去重/分页；
//! 任一分片失败或总超时整体报错，不返回部分结果。
use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use ecat_data::{RdbmsError, Row};
use serde_json::{Number, Value};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[cfg(test)]
mod tests;

use crate::catalog::ShardError;
use crate::router::ShardRouter;

/// 一次显式 scatter 的输入；`sql` 原样发给每个分片，不做改写。
pub struct ScatterRequest {
    /// 目录中的表名：决定分片集合与去重主键。
    pub table: String,
    pub sql: String,
    pub params: Vec<Value>,
    /// 归并时的排序列（升序）。
    pub order_by: String,
    pub limit: u64,
    pub offset: u64,
    /// 分片扇出/收集阶段的总超时（`tokio::time::timeout` 只包住 gather）；
    /// 收集完成后的排序/去重/分页是同步的，不受此超时约束。
    pub timeout: Duration,
    /// 同时在飞的分片数上限；必须 ≥ 1。
    pub max_concurrency: usize,
}

impl ShardRouter {
    /// 在表的所有分片上执行同一条查询并归并；任一失败或超时即整体报错。
    pub async fn scatter(&self, req: &ScatterRequest) -> Result<Vec<Row>, ShardError> {
        // 0 个许可会让整次 scatter 永远挂起；在信任边界直接拒绝。
        if req.max_concurrency == 0 {
            return Err(ShardError::Config(
                "scatter max_concurrency must be at least 1".to_owned(),
            ));
        }
        // 取一份 owned 快照，读锁不能跨 `.await`；热更新不影响本次 scatter。
        let table = self.catalog().table(&req.table)?.clone();
        let mut results: Vec<Option<Vec<Row>>> = table.shards.iter().map(|_| None).collect();
        let mut failed: Vec<String> = Vec::new();
        let permits = Arc::new(Semaphore::new(req.max_concurrency));
        let mut tasks = JoinSet::new();
        for (index, shard) in table.shards.iter().enumerate() {
            let client = self.clients.get(&shard.name).ok_or_else(|| {
                ShardError::Config(format!("shard `{}` has no client", shard.name))
            })?;
            let client = Arc::clone(client);
            let permits = Arc::clone(&permits);
            // 不做 SQL 解析/改写：同一个串、同一组参数原样发到每个分片。
            let sql = req.sql.clone();
            let params = req.params.clone();
            tasks.spawn(async move {
                let permit = match permits.acquire_owned().await {
                    Ok(permit) => permit,
                    // 只有本函数持有信号量，关闭只会在任务被丢弃时发生。
                    Err(_) => {
                        return (
                            index,
                            Err(RdbmsError::Database("scatter semaphore closed".into())),
                        );
                    }
                };
                let outcome = client.query_with(&sql, &params).await;
                drop(permit);
                (index, outcome)
            });
        }
        let gather = async {
            while let Some(joined) = tasks.join_next().await {
                match joined {
                    Ok((index, Ok(rows))) => results[index] = Some(rows),
                    Ok((index, Err(_))) => failed.push(table.shards[index].name.clone()),
                    Err(err) => {
                        return Err(ShardError::Config(format!("scatter task failed: {err}")));
                    }
                }
            }
            Ok(())
        };
        match tokio::time::timeout(req.timeout, gather).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => {
                // 未完成的分片计入失败；JoinSet 随函数返回被 drop，任务随之取消。
                for (index, slot) in results.iter().enumerate() {
                    let name = &table.shards[index].name;
                    if slot.is_none() && !failed.contains(name) {
                        failed.push(name.clone());
                    }
                }
            }
        }
        if !failed.is_empty() {
            failed.sort();
            return Err(ShardError::PartialFailure { failed });
        }
        merge(&req.order_by, &table.pk, req.offset, req.limit, results)
    }
}

/// 排序 → 按主键去重 → 分页。`results` 与目录分片同序。
fn merge(
    order_by: &str,
    pk: &str,
    offset: u64,
    limit: u64,
    results: Vec<Option<Vec<Row>>>,
) -> Result<Vec<Row>, ShardError> {
    let mut ordered: Vec<(Value, usize, Row)> = Vec::new();
    for (index, rows) in results.into_iter().enumerate() {
        let Some(rows) = rows else {
            return Err(ShardError::Config(format!(
                "scatter result missing for shard #{index}"
            )));
        };
        for row in rows {
            // NULL 的跨库排序语义不一致，放行会让归并顺序依赖分片实现。
            let Some(value) = row.get(order_by).filter(|value| !value.is_null()) else {
                return Err(ShardError::Config(format!(
                    "scatter row is missing or null order_by column `{order_by}`"
                )));
            };
            ordered.push((value.clone(), index, row));
        }
    }
    // 分片下标是次键：主键重复时保留哪一份、同值行怎么排都与完成顺序无关。
    ordered.sort_by(|(left, left_shard, _), (right, right_shard, _)| {
        compare_values(left, right).then(left_shard.cmp(right_shard))
    });
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(ordered.len());
    for (_, _, row) in ordered {
        let Some(pk_value) = row.get(pk).filter(|value| !value.is_null()) else {
            return Err(ShardError::Config(format!(
                "scatter row is missing or null pk column `{pk}`"
            )));
        };
        if seen.insert(pk_value.to_string()) {
            merged.push(row);
        }
    }
    let skip = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    Ok(merged.into_iter().skip(skip).take(take).collect())
}

/// 按 `order_by` 列升序比较。`Value` 没有 `Ord`，这里给一个与完成顺序无关的
/// 全序：不同类型先按类型序，同类型内数字按数值、字符串按字典序、其余按 JSON 文本。
fn compare_values(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
        (Value::Number(left), Value::Number(right)) => {
            compare_numbers(left, right).then_with(|| left.to_string().cmp(&right.to_string()))
        }
        (Value::String(left), Value::String(right)) => left.cmp(right),
        _ => value_rank(left)
            .cmp(&value_rank(right))
            .then_with(|| left.to_string().cmp(&right.to_string())),
    }
}

fn value_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn compare_numbers(left: &Number, right: &Number) -> Ordering {
    if let (Some(left), Some(right)) = (left.as_i64(), right.as_i64()) {
        return left.cmp(&right);
    }
    // JSON 数字不含 NaN/Infinity，`as_f64` 对任意变体都有值。
    left.as_f64()
        .partial_cmp(&right.as_f64())
        .unwrap_or(Ordering::Equal)
}

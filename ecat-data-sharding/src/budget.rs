// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分片连接池预算：所有分片客户端的池大小之和 × 部署副本数不得超过总预算。
use std::sync::Arc;

use ecat_data::RdbmsClient;

use crate::ShardError;

/// 整个部署允许的数据库连接总数。
pub struct PoolBudget {
    pub max_total_connections: u32,
}

/// 校验分片连接池总预算（纯计算，无 I/O）。
///
/// 对每个报告了 [`RdbmsClient::pool_size`] 的客户端求和，再乘 `instances`
/// （同一部署的 app 副本数）；超过 `max_total_connections` 报
/// [`ShardError::Config`]。`None`（后端不报告池大小）的客户端被跳过，
/// 错误信息里会注明被跳过的数量 —— 否则「全部未知 ⇒ 和恒为 0 ⇒ 永远通过」
/// 会被误读成验证过。
pub fn validate_pool_budget(
    clients: &[Arc<dyn RdbmsClient>],
    instances: u32,
    budget: &PoolBudget,
) -> Result<(), ShardError> {
    let mut total: u64 = 0;
    let mut unknown = 0usize;
    for client in clients {
        match client.pool_size() {
            Some(n) => total += u64::from(n),
            None => unknown += 1,
        }
    }
    let total = total * u64::from(instances);
    if total > u64::from(budget.max_total_connections) {
        let skipped = if unknown == 0 {
            String::new()
        } else {
            format!(" ({unknown} client(s) skipped: unknown pool size)")
        };
        return Err(ShardError::Config(format!(
            "connection pool budget exceeded: {total} > {}{skipped}",
            budget.max_total_connections
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

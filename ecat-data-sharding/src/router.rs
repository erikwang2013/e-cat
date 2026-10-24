// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 按键路由：目录 + 端点客户端 → 绑定到单个分片的 [`KeyedClient`]。
use std::collections::HashMap;
use std::sync::Arc;

use ecat_data::RdbmsClient;
use serde_json::Value;

#[cfg(test)]
mod tests;

use crate::catalog::{ShardCatalog, ShardError};
use crate::client::KeyedClient;

/// 键绑定路由器；`clients` 以目录端点的 `name` 为键。
pub struct ShardRouter {
    // Task 19 的 scatter 复用同一份快照，故 crate 内可见。
    pub(crate) catalog: ShardCatalog,
    pub(crate) clients: HashMap<String, Arc<dyn RdbmsClient>>,
}

impl ShardRouter {
    /// 校验目录，并要求每个端点都有客户端；缺一律报错，不猜默认分片。
    pub fn new(
        catalog: ShardCatalog,
        clients: HashMap<String, Arc<dyn RdbmsClient>>,
    ) -> Result<Self, ShardError> {
        catalog.validate()?;
        for table in &catalog.tables {
            for shard in &table.shards {
                if !clients.contains_key(&shard.name) {
                    return Err(ShardError::Config(format!(
                        "table `{}` shard `{}` has no client",
                        table.name, shard.name
                    )));
                }
            }
        }
        Ok(Self { catalog, clients })
    }

    /// 返回绑定到 `table` 上 `key` 命中分片的客户端。
    pub fn for_key(&self, table: &str, key: &Value) -> Result<KeyedClient, ShardError> {
        // `shard_for` 已按 `shards.len()` 校验过算法，下标必在界内。
        let index = self.catalog.shard_for(table, key)?;
        let shard = &self.catalog.table(table)?.shards[index];
        let client = self.clients.get(&shard.name).ok_or_else(|| {
            // 构造时已保证端点有客户端；这里只防目录与客户端表被拆开改坏。
            ShardError::Config(format!("shard `{}` has no client", shard.name))
        })?;
        Ok(KeyedClient::new(Arc::clone(client), table.to_owned()))
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 按键路由：目录 + 端点客户端 → 绑定到单个分片的 [`KeyedClient`]。
use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard};

use ecat_data::RdbmsClient;
use serde_json::Value;

#[cfg(test)]
mod tests;

use crate::catalog::{ShardCatalog, ShardError};
use crate::client::KeyedClient;

/// 键绑定路由器；`clients` 以目录端点的 `name` 为键。
pub struct ShardRouter {
    // Task 19 的 scatter 与 Task 27 的 health 复用同一份快照，故 crate 内可见。
    pub(crate) catalog: Arc<RwLock<ShardCatalog>>,
    pub(crate) clients: HashMap<String, Arc<dyn RdbmsClient>>,
}

impl ShardRouter {
    /// 校验目录，并要求每个端点都有客户端；缺一律报错，不猜默认分片。
    pub fn new(
        catalog: ShardCatalog,
        clients: HashMap<String, Arc<dyn RdbmsClient>>,
    ) -> Result<Self, ShardError> {
        catalog.validate()?;
        Self::validate_clients(&catalog, &clients)?;
        Ok(Self {
            catalog: Arc::new(RwLock::new(catalog)),
            clients,
        })
    }

    /// 热替换拓扑：先校验目录与客户端，再持写锁只做 swap。
    /// 旧的 [`KeyedClient`] 持有旧 `Arc<dyn RdbmsClient>`，不受影响。
    pub fn reload(&self, catalog: ShardCatalog) -> Result<(), ShardError> {
        catalog.validate()?;
        Self::validate_clients(&catalog, &self.clients)?;
        *self.catalog.write().unwrap_or_else(|e| e.into_inner()) = catalog;
        Ok(())
    }

    /// 当前拓扑快照。
    pub fn catalog(&self) -> ShardCatalog {
        self.read_catalog().clone()
    }

    fn read_catalog(&self) -> RwLockReadGuard<'_, ShardCatalog> {
        // 写者 panic 不应让读路径永久失败：取回被污染的内层值。
        self.catalog.read().unwrap_or_else(|e| e.into_inner())
    }

    fn validate_clients(
        catalog: &ShardCatalog,
        clients: &HashMap<String, Arc<dyn RdbmsClient>>,
    ) -> Result<(), ShardError> {
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
        Ok(())
    }

    /// 返回绑定到 `table` 上 `key` 命中分片的客户端。
    pub fn for_key(&self, table: &str, key: &Value) -> Result<KeyedClient, ShardError> {
        self.for_key_with_shard(table, key)
            .map(|(_, client)| client)
    }

    /// 同 [`ShardRouter::for_key`]，并返回命中的分片名（双写时区分正常重分片）。
    pub(crate) fn for_key_with_shard(
        &self,
        table: &str,
        key: &Value,
    ) -> Result<(String, KeyedClient), ShardError> {
        let (shard_name, client) = {
            let catalog = self.read_catalog();
            // `shard_for` 已按 `shards.len()` 校验过算法，下标必在界内。
            let index = catalog.shard_for(table, key)?;
            let shard_name = catalog.table(table)?.shards[index].name.clone();
            let client = self.clients.get(&shard_name).ok_or_else(|| {
                // 构造/热更新时已保证端点有客户端；这里只防目录与客户端表被拆开改坏。
                ShardError::Config(format!("shard `{shard_name}` has no client"))
            })?;
            (shard_name, Arc::clone(client))
        };
        Ok((shard_name, KeyedClient::new(client, table.to_owned())))
    }
}

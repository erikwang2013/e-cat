// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 拓扑源：把一份 [`ShardCatalog`] 交给 [`ShardRouter::reload`]。
//! 静态源从内存返回；远程源（Consul/etcd）按 feature 编译。
use async_trait::async_trait;

use crate::catalog::{ShardCatalog, ShardError};

#[cfg(any(feature = "consul", feature = "etcd"))]
mod remote;
#[cfg(feature = "consul")]
pub use remote::ConsulTopologySource;
#[cfg(feature = "etcd")]
pub use remote::EtcdTopologySource;

/// 拓扑加载源。调用方负责在加载后校验并 `reload`。
#[async_trait]
pub trait TopologySource: Send + Sync {
    async fn load(&self) -> Result<ShardCatalog, ShardError>;
}

/// 从内存中的目录返回 clone；用于测试和静态配置。
#[derive(Debug, Clone)]
pub struct StaticTopology {
    catalog: ShardCatalog,
}

impl StaticTopology {
    pub fn new(catalog: ShardCatalog) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl TopologySource for StaticTopology {
    async fn load(&self) -> Result<ShardCatalog, ShardError> {
        Ok(self.catalog.clone())
    }
}

#[cfg(test)]
mod tests;

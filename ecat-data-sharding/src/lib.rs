// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分库分表目录：配置类型、拓扑校验、按分片键路由、显式 scatter 归并、Snowflake 全局 ID、outbox 存储与 relay 投递、连接池预算校验。
mod budget;
mod catalog;
mod client;
mod config;
#[cfg(feature = "health")]
mod health;
mod id;
mod outbox;
mod proxy;
mod router;
mod scatter;
mod topology;

pub use budget::{PoolBudget, validate_pool_budget};
pub use catalog::{
    ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardTable, stable_hash64,
};
pub use client::KeyedClient;
pub use config::{ShardingConfig, ShardingMode};
#[cfg(feature = "health")]
pub use health::{OutboxHealthCheck, ShardHealthCheck};
pub use id::Snowflake;
pub use outbox::{
    OutboxHandler, OutboxRecord, OutboxRelay, OutboxStatus, OutboxStore, RelayReport,
};
pub use proxy::validate_proxy;
pub use router::ShardRouter;
pub use scatter::ScatterRequest;
pub use topology::{StaticTopology, TopologySource};

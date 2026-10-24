// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分库分表目录：配置类型、拓扑校验、按分片键路由、显式 scatter 归并、Snowflake 全局 ID 与 outbox 存储。
mod catalog;
mod client;
mod config;
mod id;
mod outbox;
mod proxy;
mod router;
mod scatter;

pub use catalog::{
    ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardTable, stable_hash64,
};
pub use client::KeyedClient;
pub use config::{ShardingConfig, ShardingMode};
pub use id::Snowflake;
pub use outbox::{OutboxRecord, OutboxStatus, OutboxStore};
pub use proxy::validate_proxy;
pub use router::ShardRouter;
pub use scatter::ScatterRequest;

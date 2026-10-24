// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分库分表目录：配置类型、拓扑校验、按分片键路由、显式 scatter 归并与 Snowflake 全局 ID。
mod catalog;
mod client;
mod config;
mod id;
mod proxy;
mod router;
mod scatter;

pub use catalog::{
    ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardTable, stable_hash64,
};
pub use client::KeyedClient;
pub use config::{ShardingConfig, ShardingMode};
pub use id::Snowflake;
pub use proxy::validate_proxy;
pub use router::ShardRouter;
pub use scatter::ScatterRequest;

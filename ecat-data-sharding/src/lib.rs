// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分库分表目录：配置类型、拓扑校验与按分片键路由；不含 SQL 与网络 I/O。
mod catalog;
mod config;

pub use catalog::{
    ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardTable, stable_hash64,
};
pub use config::{ShardingConfig, ShardingMode};

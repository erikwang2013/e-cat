// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分片配置：部署模式 + 目录，以及只有配置层才知道的模式约束。
use serde::Deserialize;

use crate::catalog::{ShardCatalog, ShardError};

/// 顶层 `sharding:` 配置段。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardingConfig {
    pub mode: ShardingMode,
    pub catalog: ShardCatalog,
}

/// 部署模式：`Proxy` 由分片代理决定路由，`Client` 由 e-cat 直连各分片。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardingMode {
    Proxy,
    Client,
}

impl ShardingConfig {
    /// 校验目录，并检查该模式下的端点 `url` 约束。
    pub fn validate(&self) -> Result<(), ShardError> {
        self.catalog.validate()?;
        for table in &self.catalog.tables {
            for shard in &table.shards {
                let has_url = shard
                    .url
                    .as_deref()
                    .is_some_and(|url| !url.trim().is_empty());
                match self.mode {
                    ShardingMode::Proxy if has_url => {
                        return Err(ShardError::Config(format!(
                            "table `{}` shard `{}`: proxy mode forbids per-shard urls",
                            table.name, shard.name
                        )));
                    }
                    ShardingMode::Client if !has_url => {
                        return Err(ShardError::Config(format!(
                            "table `{}` shard `{}`: client mode requires a url",
                            table.name, shard.name
                        )));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 读写分离路由的配置与构建器：一份主库配置 + 零或多份副本配置 →
//! [`RdbmsRouting`]。

use std::sync::Arc;
use std::time::Duration;

use ecat_data::{RdbmsClient, RdbmsRouting};
use serde::Deserialize;

use crate::{SqlxClient, SqlxConfig};

/// `fallback_to_primary` 的默认值。
fn default_true() -> bool {
    true
}

/// 读写分离配置。**未知键直接报错**（拼错 `replicas` 会静默退化成单主库）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlxRoutingConfig {
    pub primary: SqlxConfig,
    #[serde(default)]
    pub replicas: Vec<SqlxConfig>,
    /// 副本实测延迟超过它即跳过；`None` = 不探测。
    #[serde(default)]
    pub max_replica_lag_secs: Option<u64>,
    /// 副本全不可用时是否降级读主，默认 `true`。
    #[serde(default = "default_true")]
    pub fallback_to_primary: bool,
}

/// 按配置逐份建连并组装路由。任何一份连不上都直接返回该错误（不静默丢弃）。
pub async fn build_routing(cfg: &SqlxRoutingConfig) -> Result<RdbmsRouting, sqlx::Error> {
    let primary = Arc::new(SqlxClient::from_config(cfg.primary.clone()).await?);
    let mut replicas: Vec<Arc<dyn RdbmsClient>> = Vec::with_capacity(cfg.replicas.len());
    for replica in &cfg.replicas {
        replicas.push(Arc::new(SqlxClient::from_config(replica.clone()).await?));
    }
    let mut routing =
        RdbmsRouting::new(primary, replicas).fallback_to_primary(cfg.fallback_to_primary);
    if let Some(secs) = cfg.max_replica_lag_secs {
        routing = routing.max_replica_lag(Duration::from_secs(secs));
    }
    Ok(routing)
}

#[cfg(test)]
mod tests;

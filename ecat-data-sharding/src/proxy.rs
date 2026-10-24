// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! B 阶段代理模式校验：路由交给代理，e-cat 只保留单端点连接配置。
use crate::catalog::ShardError;
use crate::config::{ShardingConfig, ShardingMode};

#[cfg(test)]
mod tests;

/// 校验代理模式配置：仅 `Proxy`、目录非空、端点不带逐分片 `url`；
/// 目录与 `url` 规则委托给 [`ShardingConfig::validate`]，不在此处重写。
pub fn validate_proxy(cfg: &ShardingConfig) -> Result<(), ShardError> {
    if !matches!(cfg.mode, ShardingMode::Proxy) {
        return Err(ShardError::Config(
            "proxy mode required: `mode` must be `proxy`".to_owned(),
        ));
    }
    if cfg.catalog.tables.is_empty() {
        return Err(ShardError::Config(
            "proxy mode requires at least one catalog table".to_owned(),
        ));
    }
    // 目录完整性与「Proxy 模式禁止逐分片 url」都在这里。
    cfg.validate()
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    Standalone,
    Distributed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    #[default]
    Memory,
    Redis,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentConfig {
    pub mode: DeploymentMode,
    pub node_id: String,
    #[serde(default)]
    pub worker_id: Option<u16>,
    #[serde(default)]
    pub cache: CacheMode,
    #[serde(default)]
    pub sharding_enabled: bool,
    #[serde(default)]
    pub tiering_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentError {
    StandaloneWithSharding,
    DistributedWithoutNodeId,
    DistributedWithMemoryCache,
    InvalidWorkerId,
}

impl fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::StandaloneWithSharding => "standalone deployment cannot enable sharding",
            Self::DistributedWithoutNodeId => "distributed deployment requires a non-empty node_id",
            Self::DistributedWithMemoryCache => "distributed deployment requires the redis cache",
            Self::InvalidWorkerId => "distributed sharding requires worker_id < 1024",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for DeploymentError {}

impl DeploymentConfig {
    pub fn validate(&self) -> Result<(), DeploymentError> {
        match self.mode {
            DeploymentMode::Standalone => {
                if self.sharding_enabled {
                    return Err(DeploymentError::StandaloneWithSharding);
                }
            }
            DeploymentMode::Distributed => {
                if self.node_id.trim().is_empty() {
                    return Err(DeploymentError::DistributedWithoutNodeId);
                }
                if self.cache != CacheMode::Redis {
                    return Err(DeploymentError::DistributedWithMemoryCache);
                }
                if self.sharding_enabled && !matches!(self.worker_id, Some(id) if id < 1024) {
                    return Err(DeploymentError::InvalidWorkerId);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

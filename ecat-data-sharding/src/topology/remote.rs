// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 远程拓扑源：Consul 阻塞查询 / etcd 轮询；坏数据保留最后已知拓扑。
use std::collections::HashMap;

use async_trait::async_trait;
use ecat_config_remote::ConfigSource;
use serde_json::Value;
use tokio::sync::watch;

use super::TopologySource;
use crate::catalog::{ShardCatalog, ShardError};

#[cfg(feature = "consul")]
use ecat_config_remote::ConsulConfigSource;
#[cfg(feature = "etcd")]
use ecat_config_remote::EtcdConfigSource;
#[cfg(feature = "etcd")]
use std::time::Duration;

/// 拓扑 JSON 在远程 KV 前缀下的固定键名。
const TOPOLOGY_KEY: &str = "topology";
#[cfg(feature = "etcd")]
const DEFAULT_WATCH_INTERVAL: Duration = Duration::from_secs(30);

/// 从远程配置 map 取出拓扑 JSON；缺失即 fail-closed。
fn topology_value(map: &HashMap<String, Value>) -> Result<Value, ShardError> {
    map.get(TOPOLOGY_KEY)
        .cloned()
        .ok_or_else(|| ShardError::Config(format!("missing `{TOPOLOGY_KEY}` key in remote config")))
}

fn parse_catalog(value: Value) -> Result<ShardCatalog, ShardError> {
    let catalog: ShardCatalog = serde_json::from_value(value)
        .map_err(|e| ShardError::Config(format!("invalid shard topology JSON: {e}")))?;
    catalog.validate()?;
    Ok(catalog)
}

/// Consul KV 远程拓扑源。拓扑 JSON 位于 `<prefix>/topology`。
#[cfg(feature = "consul")]
pub struct ConsulTopologySource {
    source: ConsulConfigSource,
}

#[cfg(feature = "consul")]
impl ConsulTopologySource {
    pub fn new(base_url: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            source: ConsulConfigSource::new(base_url, prefix),
        }
    }

    /// Consul ACL token。
    pub fn with_token(self, token: impl Into<String>) -> Self {
        Self {
            source: self.source.token(token),
        }
    }

    /// 启动 watch：首次加载失败即返回错误（fail-closed）；
    /// 后续刷新失败保留最后已知拓扑。
    pub async fn watch(&self) -> Result<watch::Receiver<ShardCatalog>, ShardError> {
        let initial = self.load().await?;
        let (tx, rx) = watch::channel(initial);
        let mut events = self.source.watch();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let result = event
                    .map_err(|e| ShardError::Config(format!("consul topology: {e}")))
                    .and_then(|map| topology_value(&map))
                    .and_then(parse_catalog);
                match result {
                    Ok(catalog) => {
                        if tx.send(catalog).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "consul topology refresh failed; keeping last known")
                    }
                }
            }
        });
        Ok(rx)
    }
}

#[cfg(feature = "consul")]
#[async_trait]
impl TopologySource for ConsulTopologySource {
    async fn load(&self) -> Result<ShardCatalog, ShardError> {
        let map = self
            .source
            .load()
            .await
            .map_err(|e| ShardError::Config(format!("consul topology: {e}")))?;
        parse_catalog(topology_value(&map)?)
    }
}

/// etcd KV 远程拓扑源。拓扑 JSON 位于 `<prefix>/topology`；首版轮询
/// `/v3/kv/range`，后续可换 `/v3/watch` 流。
#[cfg(feature = "etcd")]
pub struct EtcdTopologySource {
    source: EtcdConfigSource,
    watch_interval: Duration,
}

#[cfg(feature = "etcd")]
impl EtcdTopologySource {
    pub fn new(endpoints: Vec<String>, prefix: impl Into<String>) -> Self {
        Self {
            source: EtcdConfigSource::new(endpoints, prefix),
            watch_interval: DEFAULT_WATCH_INTERVAL,
        }
    }

    pub fn with_auth(self, user: &str, password: &str) -> Self {
        Self {
            source: self.source.with_auth(user, password),
            watch_interval: self.watch_interval,
        }
    }

    pub fn with_watch_interval(mut self, interval: Duration) -> Self {
        self.watch_interval = interval;
        self
    }

    async fn load_value(&self) -> Result<Value, ShardError> {
        let map = self
            .source
            .load()
            .await
            .map_err(|e| ShardError::Config(format!("etcd topology: {e}")))?;
        topology_value(&map)
    }

    /// 启动轮询 watch：首次加载失败即返回错误（fail-closed）；
    /// 后续失败或坏数据保留最后已知拓扑，值未变化时不重复推送。
    pub async fn watch(&self) -> Result<watch::Receiver<ShardCatalog>, ShardError> {
        let initial_value = self.load_value().await?;
        let initial = parse_catalog(initial_value.clone())?;
        let (tx, rx) = watch::channel(initial);
        let source = self.source.clone();
        let interval = self.watch_interval;
        tokio::spawn(async move {
            let mut last = Some(initial_value);
            loop {
                tokio::time::sleep(interval).await;
                let map = match source.load().await {
                    Ok(map) => map,
                    Err(error) => {
                        tracing::warn!(%error, "etcd topology poll failed; keeping last known");
                        continue;
                    }
                };
                let value = match topology_value(&map) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(%error, "etcd topology poll failed; keeping last known");
                        continue;
                    }
                };
                if last.as_ref() == Some(&value) {
                    continue;
                }
                match parse_catalog(value.clone()) {
                    Ok(catalog) => {
                        last = Some(value);
                        if tx.send(catalog).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "etcd topology invalid; keeping last known")
                    }
                }
            }
        });
        Ok(rx)
    }
}

#[cfg(feature = "etcd")]
#[async_trait]
impl TopologySource for EtcdTopologySource {
    async fn load(&self) -> Result<ShardCatalog, ShardError> {
        parse_catalog(self.load_value().await?)
    }
}

#[cfg(test)]
mod tests;

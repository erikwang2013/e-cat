// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "health"：分片拓扑/连通性与 outbox 积压探针，接 `/ready` 的 readyz。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_health::HealthCheck;
use tokio::sync::Mutex;

use crate::{OutboxStore, ShardError, ShardRouter};

#[cfg(test)]
mod tests;

/// 探针语句：只验证分片答得上话，不碰任何业务表。
const PROBE_SQL: &str = "SELECT 1";

impl ShardRouter {
    /// 拓扑校验 + 每个分片 `SELECT 1`；整次探针共享一个总超时，
    /// 超时错误指出当时正在探的分片。
    pub async fn check_health(&self, timeout: Duration) -> Result<(), ShardError> {
        self.catalog.validate()?;
        let current = Arc::new(Mutex::new(None::<String>));
        let probe = {
            let current = Arc::clone(&current);
            async move {
                for table in &self.catalog.tables {
                    for shard in &table.shards {
                        *current.lock().await = Some(shard.name.clone());
                        let client = self.clients.get(&shard.name).ok_or_else(|| {
                            ShardError::Config(format!("shard `{}` has no client", shard.name))
                        })?;
                        client.query(PROBE_SQL).await.map_err(|e| {
                            ShardError::Database(format!("shard `{}`: {e}", shard.name))
                        })?;
                    }
                }
                Ok(())
            }
        };
        match tokio::time::timeout(timeout, probe).await {
            Ok(result) => result,
            Err(_) => {
                let shard = current.lock().await.clone();
                Err(ShardError::Database(match shard {
                    Some(name) => {
                        format!("shard `{name}` health probe timed out after {timeout:?}")
                    }
                    None => format!("shard health check timed out after {timeout:?}"),
                }))
            }
        }
    }
}

/// 分片健康检查：拓扑有效且每个分片都答得上 `SELECT 1`。
pub struct ShardHealthCheck {
    router: Arc<ShardRouter>,
    timeout: Duration,
}

impl ShardHealthCheck {
    /// `timeout` 是全部探针的总预算。
    pub fn new(router: Arc<ShardRouter>, timeout: Duration) -> Self {
        Self { router, timeout }
    }
}

#[async_trait]
impl HealthCheck for ShardHealthCheck {
    fn name(&self) -> &str {
        "data.shards"
    }

    async fn check(&self) -> Result<(), String> {
        self.router
            .check_health(self.timeout)
            .await
            .map_err(|e| e.to_string())
    }
}

/// outbox 积压检查：未完成条数超过阈值即不健康。
///
/// 自身不带超时；后端负责兜底（`ecat-data-sqlx` 池默认 `query_timeout` 为 30s）。
pub struct OutboxHealthCheck {
    store: Arc<OutboxStore>,
    max_backlog: u64,
}

impl OutboxHealthCheck {
    pub fn new(store: Arc<OutboxStore>, max_backlog: u64) -> Self {
        Self { store, max_backlog }
    }
}

#[async_trait]
impl HealthCheck for OutboxHealthCheck {
    fn name(&self) -> &str {
        "data.outbox"
    }

    async fn check(&self) -> Result<(), String> {
        let pending = self
            .store
            .pending_count()
            .await
            .map_err(|e| e.to_string())?;
        if pending > self.max_backlog {
            return Err(format!(
                "outbox backlog {pending} exceeds {}",
                self.max_backlog
            ));
        }
        Ok(())
    }
}

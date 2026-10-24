// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "health"：冷层可达性探针，接 `/ready` 的 readyz。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_health::HealthCheck;

use crate::ColdSink;

#[cfg(test)]
mod tests;

/// 冷层健康检查。
pub struct TieringHealthCheck {
    cold: Arc<dyn ColdSink>,
    max_lag: Duration,
}

impl TieringHealthCheck {
    /// `max_lag` 是冷层探针的应答预算，超过即视为不可达。
    // shortcut: 真正的归档滞后在 ArchiveStateStore 里，本接口只拿到 ColdSink，
    // 判不了；要按「最近一次成功归档」算滞后时再注入状态存储。
    pub fn new(cold: Arc<dyn ColdSink>, max_lag: Duration) -> Self {
        Self { cold, max_lag }
    }
}

#[async_trait]
impl HealthCheck for TieringHealthCheck {
    fn name(&self) -> &str {
        "data.tiering"
    }

    async fn check(&self) -> Result<(), String> {
        match tokio::time::timeout(self.max_lag, self.cold.health_check()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("cold sink: {e}")),
            Err(_) => Err(format!(
                "cold sink did not answer within {:?}",
                self.max_lag
            )),
        }
    }
}

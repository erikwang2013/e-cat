// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "health"：冷层可达性探针，接 `/ready` 的 readyz。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_health::HealthCheck;

use crate::ColdSink;

#[cfg(test)]
mod tests;

/// 冷层可达性检查：`ColdSink::health_check` 在预算内应答即健康。
pub struct TieringHealthCheck {
    cold: Arc<dyn ColdSink>,
    probe_timeout: Duration,
}

impl TieringHealthCheck {
    /// `probe_timeout` 是冷层探针的应答预算，超过即视为不可达。
    /// 归档滞后检测不在本探针内：`ColdSink` 没有滞后信号，需要状态存储。
    pub fn new(cold: Arc<dyn ColdSink>, probe_timeout: Duration) -> Self {
        Self {
            cold,
            probe_timeout,
        }
    }
}

#[async_trait]
impl HealthCheck for TieringHealthCheck {
    fn name(&self) -> &str {
        "data.tiering"
    }

    async fn check(&self) -> Result<(), String> {
        match tokio::time::timeout(self.probe_timeout, self.cold.health_check()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("cold sink: {e}")),
            Err(_) => Err(format!(
                "cold sink did not answer within {:?}",
                self.probe_timeout
            )),
        }
    }
}

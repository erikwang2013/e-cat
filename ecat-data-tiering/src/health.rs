// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "health"：冷层可达性探针，接 `/ready` 的 readyz。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_health::HealthCheck;

use crate::{ArchiveStateStore, ColdSink};

#[cfg(test)]
mod tests;

/// 冷层可达性检查：`ColdSink::health_check` 在预算内应答即健康。
pub struct TieringHealthCheck {
    cold: Arc<dyn ColdSink>,
    probe_timeout: Duration,
    archive_lag: Option<ArchiveLag>,
}

struct ArchiveLag {
    store: Arc<ArchiveStateStore>,
    max_lag: time::Duration,
}

impl TieringHealthCheck {
    /// `probe_timeout` 是冷层探针的应答预算，超过即视为不可达。
    /// 归档滞后检测不在本探针内：`ColdSink` 没有滞后信号，需要状态存储。
    pub fn new(cold: Arc<dyn ColdSink>, probe_timeout: Duration) -> Self {
        Self {
            cold,
            probe_timeout,
            archive_lag: None,
        }
    }

    /// 追加归档滞后检查：按状态表里出现过的表，最新 `updated_at` 超过
    /// `max_lag` 即不健康；没有状态行视为「尚无归档历史」，保持健康。
    pub fn with_archive_lag(
        mut self,
        store: Arc<ArchiveStateStore>,
        max_lag: time::Duration,
    ) -> Self {
        self.archive_lag = Some(ArchiveLag { store, max_lag });
        self
    }
}

#[async_trait]
impl HealthCheck for TieringHealthCheck {
    fn name(&self) -> &str {
        "data.tiering"
    }

    async fn check(&self) -> Result<(), String> {
        match tokio::time::timeout(self.probe_timeout, self.cold.health_check()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("cold sink: {e}")),
            Err(_) => {
                return Err(format!(
                    "cold sink did not answer within {:?}",
                    self.probe_timeout
                ));
            }
        }
        if let Some(lag) = &self.archive_lag {
            check_archive_lag(lag).await?;
        }
        Ok(())
    }
}

async fn check_archive_lag(lag: &ArchiveLag) -> Result<(), String> {
    let latest = lag
        .store
        .latest_per_table()
        .await
        .map_err(|e| format!("archive state: {e}"))?;
    let now = time::OffsetDateTime::now_utc();
    for (table, latest) in latest {
        let age = now - latest;
        if age > lag.max_lag {
            return Err(format!(
                "archive lag for `{table}` is {age:?}, exceeds {:?}",
                lag.max_lag
            ));
        }
    }
    Ok(())
}

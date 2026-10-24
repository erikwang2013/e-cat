// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! health 测试：冷层可达性与探针预算；假 sink，不连真存储。
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_data::Row;
use ecat_health::HealthCheck;

use super::TieringHealthCheck;
use crate::{ColdSink, TieringError};

struct FakeSink {
    fail: bool,
    delay: Duration,
}

impl FakeSink {
    fn ok() -> Self {
        Self {
            fail: false,
            delay: Duration::ZERO,
        }
    }
}

#[async_trait]
impl ColdSink for FakeSink {
    async fn write_chunk(
        &self,
        _table: &str,
        _partition: &str,
        _chunk: u64,
        _rows: &[Row],
    ) -> Result<String, TieringError> {
        Err(TieringError::Cold("not used by health tests".into()))
    }

    async fn verify_chunk(
        &self,
        _table: &str,
        _partition: &str,
        _chunk: u64,
        _checksum: &str,
    ) -> Result<bool, TieringError> {
        Err(TieringError::Cold("not used by health tests".into()))
    }

    async fn health_check(&self) -> Result<(), TieringError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if self.fail {
            Err(TieringError::Cold("clickhouse unreachable".into()))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn tiering_health_fails_when_cold_store_unreachable() {
    let sink = FakeSink {
        fail: true,
        ..FakeSink::ok()
    };
    let check = TieringHealthCheck::new(Arc::new(sink), Duration::from_secs(1));

    assert_eq!(check.name(), "data.tiering");
    let err = check.check().await.unwrap_err();
    assert!(err.contains("cold"), "{err}");
}

#[tokio::test]
async fn tiering_health_ok_when_cold_store_answers() {
    let check = TieringHealthCheck::new(Arc::new(FakeSink::ok()), Duration::from_secs(1));

    assert_eq!(check.check().await, Ok(()));
}

#[tokio::test]
async fn tiering_health_fails_when_probe_exceeds_budget() {
    let sink = FakeSink {
        delay: Duration::from_secs(30),
        ..FakeSink::ok()
    };
    let check = TieringHealthCheck::new(Arc::new(sink), Duration::from_millis(20));

    let err = check.check().await.unwrap_err();
    assert!(err.contains("cold sink"), "{err}");
}

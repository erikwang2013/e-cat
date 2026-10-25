// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! CLI 接线测试：fake hot/cold + SQLite 状态库，不连 live DB。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use ecat_data_sqlx::{PoolParams, SqlxClient};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{CliConfig, CliDeps, Command, run_with};
use crate::{
    ArchiveJobState, ArchivePhase, ArchiveStateStore, ColdSink, TableTieringConfig, TieringError,
};

struct FakeHot {
    queried: AtomicBool,
}

impl FakeHot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queried: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl SqlExecutor for FakeHot {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        Ok(0)
    }

    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        Ok(Vec::new())
    }

    async fn query_with(&self, sql: &str, _params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        if sql.contains("information_schema.PARTITIONS") {
            self.queried.store(true, Ordering::Relaxed);
        }
        Ok(Vec::new())
    }

    fn dialect(&self) -> Dialect {
        Dialect::MySql
    }
}

#[async_trait]
impl RdbmsClient for FakeHot {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Ok(Transaction::new())
    }
}

struct FakeSink {
    verify: bool,
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
        Ok("checksum".into())
    }

    async fn verify_chunk(
        &self,
        _table: &str,
        _partition: &str,
        _chunk: u64,
        _checksum: &str,
    ) -> Result<bool, TieringError> {
        Ok(self.verify)
    }

    async fn health_check(&self) -> Result<(), TieringError> {
        Ok(())
    }
}

struct FakeDeps {
    hot: Arc<dyn RdbmsClient>,
    state: ArchiveStateStore,
    sink: Arc<dyn ColdSink>,
}

impl CliDeps for FakeDeps {
    fn hot(&self) -> Arc<dyn RdbmsClient> {
        self.hot.clone()
    }

    fn state(&self) -> ArchiveStateStore {
        self.state.clone()
    }

    fn sink(&self, _table: &TableTieringConfig) -> Result<Arc<dyn ColdSink>, TieringError> {
        Ok(self.sink.clone())
    }
}

async fn fresh_state() -> ArchiveStateStore {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    let db = Arc::new(
        SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap(),
    );
    let store = ArchiveStateStore::new(db);
    store.ensure_schema().await.unwrap();
    store
}

fn config() -> CliConfig {
    serde_json::from_value(json!({
        "database": {"url": "sqlite::memory:"},
        "tiering": {
            "enabled": true,
            "tables": [{
                "name": "events",
                "time_col": "ts",
                "id_col": "id",
                "retention_days": 365,
                "target": "clickhouse"
            }]
        }
    }))
    .unwrap()
}

#[test]
fn cli_config_parses_database_and_tiering() {
    let config: CliConfig = serde_json::from_value(json!({
        "database": {"url": "sqlite::memory:"},
        "tiering": {"tables": []}
    }))
    .unwrap();
    assert_eq!(config.database.url, "sqlite::memory:");
    assert!(config.tiering.tables.is_empty());
}

#[cfg(feature = "clickhouse")]
#[test]
fn clickhouse_auth_requires_username_and_password_together() {
    use super::clickhouse_auth;

    assert_eq!(clickhouse_auth(None, None).unwrap(), None);
    assert_eq!(
        clickhouse_auth(Some("user".into()), Some("secret".into())).unwrap(),
        Some(("user".into(), "secret".into()))
    );

    let missing_password = clickhouse_auth(Some("user".into()), None).unwrap_err();
    assert!(
        missing_password
            .to_string()
            .contains("ECAT_CLICKHOUSE_PASSWORD"),
        "lone username must name the missing password: {missing_password}"
    );

    let missing_username = clickhouse_auth(None, Some("secret".into())).unwrap_err();
    assert!(
        missing_username
            .to_string()
            .contains("ECAT_CLICKHOUSE_USERNAME"),
        "lone password must name the missing username: {missing_username}"
    );

    // 空字符串视为未设置。
    assert_eq!(
        clickhouse_auth(Some(String::new()), Some(String::new())).unwrap(),
        None,
        "both empty must mean no auth"
    );
    let empty_username = clickhouse_auth(Some(String::new()), Some("secret".into())).unwrap_err();
    assert!(
        empty_username
            .to_string()
            .contains("ECAT_CLICKHOUSE_USERNAME"),
        "empty username must name the missing username: {empty_username}"
    );
    let empty_password = clickhouse_auth(Some("user".into()), Some(String::new())).unwrap_err();
    assert!(
        empty_password
            .to_string()
            .contains("ECAT_CLICKHOUSE_PASSWORD"),
        "empty password must name the missing password: {empty_password}"
    );
}

#[test]
fn cli_rejects_unknown_key() {
    let result: Result<CliConfig, _> = serde_json::from_value(json!({
        "database": {"url": "sqlite::memory:"},
        "tiering": {"tables": []},
        "bogus": true
    }));
    assert!(result.is_err(), "unknown top-level key must be rejected");
}

#[tokio::test]
async fn cli_run_dispatches_to_archive_job() {
    let config = config();
    let hot = FakeHot::new();
    let deps = FakeDeps {
        hot: hot.clone(),
        state: fresh_state().await,
        sink: Arc::new(FakeSink { verify: true }),
    };
    run_with(&config, Command::Run, false, &deps)
        .await
        .expect("run dispatch");
    assert!(
        hot.queried.load(Ordering::Relaxed),
        "ArchiveJob must query MySQL partitions"
    );
}

#[tokio::test]
async fn cli_verify_reports_checksum_mismatch() {
    let config = config();
    let state = fresh_state().await;
    state
        .upsert(&ArchiveJobState {
            table: "events".into(),
            partition: "p202501".into(),
            chunk: 1,
            low_id: 1,
            high_id: 2,
            rows: 2,
            checksum: Some("checksum".into()),
            phase: ArchivePhase::Copied,
            updated_at: OffsetDateTime::now_utc(),
        })
        .await
        .unwrap();
    let deps = FakeDeps {
        hot: FakeHot::new(),
        state,
        sink: Arc::new(FakeSink { verify: false }),
    };
    let err = run_with(&config, Command::Verify, false, &deps)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("cold verify mismatch"),
        "got: {err}"
    );
    assert!(err.to_string().contains("events"), "got: {err}");
}

#[tokio::test]
async fn cli_restore_still_requires_staging() {
    let config = config();
    let deps = FakeDeps {
        hot: FakeHot::new(),
        state: fresh_state().await,
        sink: Arc::new(FakeSink { verify: true }),
    };
    let err = run_with(&config, Command::Restore, false, &deps)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("staging"), "got: {err}");
    assert!(err.to_string().contains("restore_partition"), "got: {err}");
}

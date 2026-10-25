// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-tier` 运行接线：run/verify 的真实依赖在这里组装，restore 保持显式。
use std::sync::Arc;

use ecat_data::RdbmsClient;
use ecat_data_sqlx::{SqlxClient, SqlxConfig};
use serde::Deserialize;

use crate::{
    ArchiveJob, ArchiveStateStore, ColdSink, ColdTarget, TableTieringConfig, TieringConfig,
    TieringError,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliConfig {
    pub database: SqlxConfig,
    pub tiering: TieringConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Verify,
    Restore,
}

/// 真实依赖与测试 fake 之间的唯一接缝。
pub(crate) trait CliDeps: Send + Sync {
    fn hot(&self) -> Arc<dyn RdbmsClient>;
    fn state(&self) -> ArchiveStateStore;
    fn sink(&self, table: &TableTieringConfig) -> Result<Arc<dyn ColdSink>, TieringError>;
}

pub async fn run_cli(
    config: CliConfig,
    command: Command,
    dry_run: bool,
) -> Result<(), TieringError> {
    let client = Arc::new(
        SqlxClient::from_config(config.database.clone())
            .await
            .map_err(|e| TieringError::Config(format!("database connect: {e}")))?,
    );
    let hot: Arc<dyn RdbmsClient> = client.clone();
    let state = ArchiveStateStore::new(client);
    let deps = LiveDeps {
        hot,
        state,
        verify_sample: config.tiering.verify_sample,
    };
    run_with(&config, command, dry_run, &deps).await
}

pub(crate) async fn run_with(
    config: &CliConfig,
    command: Command,
    dry_run: bool,
    deps: &dyn CliDeps,
) -> Result<(), TieringError> {
    match command {
        Command::Run => run_archive(config, dry_run, deps).await,
        Command::Verify => verify_archive(config, deps).await,
        Command::Restore => Err(TieringError::Config(
            "restore requires the operator to load cold data into a staging table first; \
             call `prepare_staging` + `restore_partition` after the staging load"
                .into(),
        )),
    }
}

async fn run_archive(
    config: &CliConfig,
    dry_run: bool,
    deps: &dyn CliDeps,
) -> Result<(), TieringError> {
    let mut tiering = config.tiering.clone();
    tiering.dry_run |= dry_run;
    tiering.validate()?;
    if !tiering.enabled {
        println!("tiering is disabled; nothing to do");
        return Ok(());
    }
    let hot = deps.hot();
    let state = deps.state();
    state.ensure_schema().await?;
    for table in &tiering.tables {
        let sink = deps.sink(table)?;
        let mut one = tiering.clone();
        one.tables = vec![table.clone()];
        let job = ArchiveJob::new(hot.clone(), sink, state.clone(), one, None);
        let report = job.run_once().await?;
        println!(
            "tier `{}`: chunks={} rows={} dropped={} skipped={}",
            table.name, report.chunks, report.rows, report.dropped_partitions, report.skipped
        );
    }
    Ok(())
}

async fn verify_archive(config: &CliConfig, deps: &dyn CliDeps) -> Result<(), TieringError> {
    config.tiering.validate()?;
    let state = deps.state();
    state.ensure_schema().await?;
    let jobs = state.list(None).await?;
    let mut mismatches = Vec::new();
    for job in &jobs {
        let Some(checksum) = job.checksum.as_deref() else {
            continue;
        };
        let table = config
            .tiering
            .tables
            .iter()
            .find(|table| table.name == job.table)
            .ok_or_else(|| {
                TieringError::Config(format!(
                    "archive job table `{}` is not in the tiering config",
                    job.table
                ))
            })?;
        let sink = deps.sink(table)?;
        if !sink
            .verify_chunk(&job.table, &job.partition, job.chunk, checksum)
            .await?
        {
            mismatches.push(format!(
                "{} partition `{}` chunk {}",
                job.table, job.partition, job.chunk
            ));
        }
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(TieringError::Cold(format!(
            "cold verify mismatch ({}): {}",
            mismatches.len(),
            mismatches.join(", ")
        )))
    }
}

struct LiveDeps {
    hot: Arc<dyn RdbmsClient>,
    state: ArchiveStateStore,
    #[cfg_attr(not(any(feature = "clickhouse", feature = "s3")), allow(dead_code))]
    verify_sample: Option<f32>,
}

impl CliDeps for LiveDeps {
    fn hot(&self) -> Arc<dyn RdbmsClient> {
        self.hot.clone()
    }

    fn state(&self) -> ArchiveStateStore {
        self.state.clone()
    }

    fn sink(&self, table: &TableTieringConfig) -> Result<Arc<dyn ColdSink>, TieringError> {
        match table.target {
            ColdTarget::Clickhouse => self.clickhouse_sink(table),
            ColdTarget::S3 => self.s3_sink(table),
        }
    }
}

impl LiveDeps {
    #[cfg(feature = "clickhouse")]
    fn clickhouse_sink(
        &self,
        table: &TableTieringConfig,
    ) -> Result<Arc<dyn ColdSink>, TieringError> {
        use crate::ClickhouseSink;
        use crate::ClickhouseTable;
        use ecat_data_clickhouse::ClickhouseClient;

        let url = env_required("ECAT_CLICKHOUSE_URL")?;
        let database =
            std::env::var("ECAT_CLICKHOUSE_DATABASE").unwrap_or_else(|_| "default".into());
        let sink = ClickhouseSink::new(
            ClickhouseClient::new(url, database),
            vec![ClickhouseTable {
                name: table.name.clone(),
                id_col: table.id_col.clone(),
            }],
            self.verify_sample,
        )?;
        Ok(Arc::new(sink))
    }

    #[cfg(not(feature = "clickhouse"))]
    fn clickhouse_sink(
        &self,
        _table: &TableTieringConfig,
    ) -> Result<Arc<dyn ColdSink>, TieringError> {
        Err(TieringError::Config(
            "ClickHouse target requires the `clickhouse` feature".into(),
        ))
    }

    #[cfg(feature = "s3")]
    fn s3_sink(&self, table: &TableTieringConfig) -> Result<Arc<dyn ColdSink>, TieringError> {
        use crate::S3Sink;
        use crate::S3Table;
        use ecat_data::StorageClient;
        use ecat_data_s3::{S3Client, S3Config};

        let client = S3Client::from_config(S3Config {
            endpoint: env_required("ECAT_S3_ENDPOINT")?,
            region: env_required("ECAT_S3_REGION")?,
            access_key: env_required("ECAT_S3_ACCESS_KEY")?,
            secret_key: env_required("ECAT_S3_SECRET_KEY")?,
            tls: None,
            query_timeout_secs: None,
            breaker: None,
            max_concurrency: None,
        })
        .map_err(|e| TieringError::Cold(format!("s3 client: {e}")))?;
        let client: Arc<dyn StorageClient> = Arc::new(client);
        let sink = S3Sink::new(
            client,
            env_required("ECAT_S3_BUCKET")?,
            std::env::var("ECAT_S3_PREFIX").unwrap_or_default(),
            vec![S3Table {
                name: table.name.clone(),
                id_col: table.id_col.clone(),
            }],
            self.verify_sample,
        )?;
        Ok(Arc::new(sink))
    }

    #[cfg(not(feature = "s3"))]
    fn s3_sink(&self, _table: &TableTieringConfig) -> Result<Arc<dyn ColdSink>, TieringError> {
        Err(TieringError::Config(
            "S3 target requires the `s3` feature".into(),
        ))
    }
}

#[cfg_attr(not(any(feature = "clickhouse", feature = "s3")), allow(dead_code))]
fn env_required(name: &str) -> Result<String, TieringError> {
    std::env::var(name)
        .map_err(|_| TieringError::Config(format!("missing environment variable `{name}`")))
}

#[cfg(test)]
mod tests;

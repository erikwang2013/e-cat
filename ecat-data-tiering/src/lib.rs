// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 冷热分层：配置与校验、归档任务状态机、归档主循环、冷层 sink 抽象与跨边界查询。
mod archive;
mod config;
mod partition;
mod query;
mod sink;
mod state;

pub use archive::{ArchiveJob, ArchiveReport, ColdSink, Pacer};
pub use config::{ColdTarget, TableTieringConfig, TieringConfig, TieringError, validate_ident};
pub use partition::{
    PartitionSpec, RestorePlan, RestoreSource, drop_partition, plan_range_partitions,
    prepare_staging, restore_partition,
};
pub use query::{ColdQuery, TieredQuery, TieredTable};
pub use sink::render_sql_value;
#[cfg(feature = "clickhouse")]
pub use sink::{ClickhouseSink, ClickhouseTable};
#[cfg(feature = "s3")]
pub use sink::{S3Sink, S3Table};
pub use state::{ArchiveJobState, ArchivePhase, ArchiveStateStore};

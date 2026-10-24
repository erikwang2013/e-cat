// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 冷热分层：配置与校验、归档任务状态机、归档主循环与冷层 sink 抽象；跨边界查询随后续任务加入。
mod archive;
mod config;
mod sink;
mod state;

pub use archive::{ArchiveJob, ArchiveReport, ColdSink, Pacer};
pub use config::{ColdTarget, TableTieringConfig, TieringConfig, TieringError, validate_ident};
pub use sink::render_sql_value;
#[cfg(feature = "clickhouse")]
pub use sink::{ClickhouseSink, ClickhouseTable};
pub use state::{ArchiveJobState, ArchivePhase, ArchiveStateStore};

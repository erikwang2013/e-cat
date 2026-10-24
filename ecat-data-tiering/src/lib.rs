// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 冷热分层：本期只有配置与校验，归档任务、冷层 sink 与跨边界查询随后续任务加入。
mod config;

pub use config::{ColdTarget, TableTieringConfig, TieringConfig, TieringError, validate_ident};

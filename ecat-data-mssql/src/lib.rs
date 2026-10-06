// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

mod config;
#[cfg(test)]
mod tests;

pub use config::{MssqlConfig, MssqlParams};

/// Microsoft SQL Server 后端。
///
/// 用 `tiberius-ng`（TDS 驱动）+ `deadpool`（连接池）实现 [`ecat_data::SqlExecutor`]。
/// 驱动细节全部封装在本 crate 内，`tiberius_ng::Client` 类型不外泄。
pub struct MssqlClient;

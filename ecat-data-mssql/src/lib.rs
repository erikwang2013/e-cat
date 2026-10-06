// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! Microsoft SQL Server 后端。
//!
//! 用 `tiberius-ng`（TDS 驱动）+ `deadpool`（连接池）实现 [`ecat_data::SqlExecutor`]。
//! 驱动细节全部封装在本 crate 内，`tiberius::Client` 类型不外泄
//! （包名 `tiberius-ng`，lib 名 `tiberius`，故代码里写 `use tiberius::...`）。
//!
//! 与 `ecat-data-sqlx` 的两处结构差异，都源于驱动形态：
//!
//! - **参数占位符是 `@P1..@Pn`**（tiberius 的 `sp_executesql` RPC），不是 `$n`/`?`。
//! - **事务没有对象**：`begin_transaction` / `commit_transaction` /
//!   `rollback_transaction` 都是连接上的状态。于是 [`ecat_data::RdbmsClient::transaction`]
//!   的 wrapper 持整条池连接，而「连接上还开着事务就被 drop」这件事只能由
//!   `MssqlManager` 在取用时清掉（sqlx 那边是 `Transaction` drop 时自动回滚）。

mod bind;
mod cell;
mod client;
mod config;
#[cfg(test)]
mod live_tests;
mod pool;
#[cfg(test)]
mod tests;
mod url_query;

pub use client::MssqlClient;
pub use config::{MssqlConfig, MssqlParams};
pub use pool::MssqlManager;

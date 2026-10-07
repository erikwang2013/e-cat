// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
mod breaker;
mod cache;
mod dialect;
mod document;
mod graph;
mod rdbms;
mod routing;
mod search;
mod storage;
mod timeout;
mod tsdb;

pub use breaker::{CircuitBreakerExecutor, breaker_error_to_backend_error, map_breaker_error};
pub use cache::Cache;
pub use dialect::Dialect;
pub use document::DocumentClient;
pub use ecat_errors::Error;
pub use graph::GraphClient;
pub use rdbms::{RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner};
pub use routing::RdbmsRouting;
pub use search::SearchClient;
pub use storage::StorageClient;
pub use timeout::{
    BackendKind, TIMEOUTS, TRANSACTIONS_LEAKED, TimeoutError, run_with_timeout, timeout_counter,
};
pub use tsdb::{DataPoint, FieldValue, TsdbClient};

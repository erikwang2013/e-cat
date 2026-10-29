// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Cassandra CQL client, backed by the pure-Rust `scylla` driver.
//!
//! Building a session performs network I/O, so [`CassandraClient::connect`]
//! and [`CassandraClient::from_config`] are `async` — unlike the synchronous
//! RDBMS backends in this workspace. CQL has no multi-statement transactions,
//! so [`RdbmsClient::transaction`] returns an error.

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction,
    map_breaker_error, run_with_timeout,
};
use ecat_tls::TlsClientConfig;
use scylla::client::session::Session;
use scylla::client::session_builder::SessionBuilder;
use scylla::response::query_result::QueryResult;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

mod convert;
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
mod tls;

use convert::{cql_to_json, json_params};

#[derive(Debug, Clone, Deserialize)]
pub struct CassandraConfig {
    /// Contact points (`host:port`); the driver discovers the rest of the cluster.
    pub nodes: Vec<String>,
    /// Keyspace used by every connection. Unquoted, so it is case-insensitive:
    /// configure the lowercase name.
    pub keyspace: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// Per-call timeout in seconds. `0` disables the timeout; omitted = 30 s.
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// Breaker settings; omitted = conservative defaults.
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// Concurrency limit. `0` = unlimited; omitted = 32.
    #[serde(default)]
    pub max_concurrency: Option<usize>,
    /// Preferred datacenter for the default load-balancing policy.
    #[serde(default)]
    pub local_dc: Option<String>,
}

pub struct CassandraClient {
    session: Session,
    query_timeout: Option<Duration>,
    /// Per-client breaker: state belongs to the backend instance, not the process.
    breaker: Arc<Breaker>,
    /// `None` = unlimited (`max_concurrency: 0`).
    semaphore: Option<Arc<Semaphore>>,
}

/// `0` disables the timeout; omitted = 30 s.
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
    }
}

async fn build_session(cfg: &CassandraConfig) -> Result<Session, RdbmsError> {
    if cfg.nodes.is_empty() {
        return Err(RdbmsError::Config(
            "cassandra: nodes must not be empty".to_owned(),
        ));
    }
    if cfg.keyspace.is_empty() {
        return Err(RdbmsError::Config(
            "cassandra: keyspace must not be empty".to_owned(),
        ));
    }

    let mut builder = SessionBuilder::new()
        .known_nodes(&cfg.nodes)
        .use_keyspace(cfg.keyspace.clone(), false);

    match (&cfg.username, &cfg.password) {
        (Some(username), Some(password)) => {
            builder = builder.user(username.clone(), password.clone());
        }
        (None, None) => {}
        _ => {
            return Err(RdbmsError::Config(
                "cassandra: username and password must be configured together".to_owned(),
            ));
        }
    }
    if let Some(local_dc) = &cfg.local_dc {
        builder = builder.prefer_datacenter(local_dc.clone());
    }
    if let Some(tls) = &cfg.tls
        && tls.is_enabled()
    {
        builder = builder.tls_context(Some(tls::build_client_config(tls)?));
    }

    builder
        .build()
        .await
        .map_err(|e| RdbmsError::Connection(format!("cassandra: {e}")))
}

impl CassandraClient {
    /// Connect to a cluster with default settings (no auth, no TLS, 30 s timeout).
    pub async fn connect(nodes: &[String], keyspace: &str) -> Result<Self, RdbmsError> {
        Self::from_config(CassandraConfig {
            nodes: nodes.to_vec(),
            keyspace: keyspace.to_owned(),
            username: None,
            password: None,
            tls: None,
            query_timeout_secs: None,
            breaker: None,
            max_concurrency: None,
            local_dc: None,
        })
        .await
    }

    pub async fn from_config(cfg: CassandraConfig) -> Result<Self, RdbmsError> {
        let session = build_session(&cfg).await?;
        let breaker = Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()));
        // Auto-wiring: constructing a client registers the outbound metrics.
        #[cfg(feature = "metrics")]
        crate::register_outbound_metrics(Arc::clone(&breaker));
        Ok(Self {
            session,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker,
            semaphore: match cfg.max_concurrency {
                // `0` = unlimited: no semaphore (a zero permit would hang forever).
                Some(0) => None,
                Some(limit) => Some(Arc::new(Semaphore::new(limit))),
                None => Some(Arc::new(Semaphore::new(32))),
            },
        })
    }

    /// This client's breaker, used by the `metrics` feature.
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// Take a concurrency permit; `None` when concurrency is unlimited.
    async fn permit(&self) -> Option<SemaphorePermit<'_>> {
        match &self.semaphore {
            Some(semaphore) => Some(
                semaphore
                    .acquire()
                    .await
                    .expect("semaphore is never closed"),
            ),
            None => None,
        }
    }

    /// One outbound call: permit -> breaker -> timeout.
    ///
    /// The order matters: the timeout must be *inside* the breaker, otherwise
    /// a wedged backend times out by dropping the inner future and the breaker
    /// never records a failure, so it can never open. Permits are outermost
    /// because queued requests have not touched the backend yet.
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, RdbmsError>
    where
        F: std::future::Future<Output = Result<T, RdbmsError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Rdbms, self.query_timeout, fut))
            .await
            .map_err(map_breaker_error)
    }
}

fn cql_error(error: impl std::fmt::Display) -> RdbmsError {
    RdbmsError::Database(format!("cassandra: {error}"))
}

/// CQL reports no affected-row count for writes; rows are only available for
/// statements that return a result set (e.g. a SELECT sent through `execute`).
fn row_count(result: QueryResult) -> Result<u64, RdbmsError> {
    if !result.is_rows() {
        return Ok(0);
    }
    Ok(result.into_rows_result().map_err(cql_error)?.rows_num() as u64)
}

fn rows_to_data(result: QueryResult) -> Result<Vec<Row>, RdbmsError> {
    let rows = result.into_rows_result().map_err(cql_error)?;
    let columns: Vec<String> = rows
        .column_specs()
        .iter()
        .map(|spec| spec.name().to_owned())
        .collect();
    let mut out = Vec::with_capacity(rows.rows_num());
    for row in rows.rows::<scylla::value::Row>().map_err(cql_error)? {
        let row = row.map_err(cql_error)?;
        let values = row.columns.iter().map(cql_to_json).collect();
        out.push(Row::new(columns.clone(), values));
    }
    Ok(out)
}

#[async_trait]
impl SqlExecutor for CassandraClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let session = &self.session;
        self.guarded(
            async move { row_count(session.query_unpaged(sql, ()).await.map_err(cql_error)?) },
        )
        .await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let session = &self.session;
        self.guarded(async move {
            rows_to_data(session.query_unpaged(sql, ()).await.map_err(cql_error)?)
        })
        .await
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        // Parameter mapping is local; only the request goes through `guarded`.
        let values = json_params(params)?;
        let session = &self.session;
        self.guarded(async move {
            row_count(
                session
                    .query_unpaged(sql, values)
                    .await
                    .map_err(cql_error)?,
            )
        })
        .await
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        let values = json_params(params)?;
        let session = &self.session;
        self.guarded(async move {
            rows_to_data(
                session
                    .query_unpaged(sql, values)
                    .await
                    .map_err(cql_error)?,
            )
        })
        .await
    }

    /// CQL has no dialect-specific placeholder syntax in this client; the
    /// workspace has no Cassandra variant, so report the ANSI fallback.
    fn dialect(&self) -> Dialect {
        Dialect::Standard
    }
}

#[async_trait]
impl RdbmsClient for CassandraClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "Cassandra does not support multi-statement transactions".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_deserializes_all_fields() {
        let cfg: CassandraConfig = serde_json::from_str(
            r#"{
                "nodes": ["10.0.0.1:9042", "10.0.0.2:9042"],
                "keyspace": "app",
                "username": "u",
                "password": "p",
                "tls": {"ca_cert": "/etc/ca.pem"},
                "query_timeout_secs": 5,
                "breaker": {"failure_ratio": 0.5},
                "max_concurrency": 8,
                "local_dc": "dc1"
            }"#,
        )
        .unwrap();
        assert_eq!(cfg.nodes.len(), 2);
        assert_eq!(cfg.keyspace, "app");
        assert_eq!(cfg.username.as_deref(), Some("u"));
        assert_eq!(cfg.password.as_deref(), Some("p"));
        assert!(cfg.tls.is_some());
        assert_eq!(cfg.query_timeout_secs, Some(5));
        assert!(cfg.breaker.is_some());
        assert_eq!(cfg.max_concurrency, Some(8));
        assert_eq!(cfg.local_dc.as_deref(), Some("dc1"));
    }

    #[test]
    fn config_optional_fields_default_to_none() {
        let cfg: CassandraConfig =
            serde_json::from_str(r#"{"nodes":["127.0.0.1:9042"],"keyspace":"ks"}"#).unwrap();
        assert!(cfg.username.is_none());
        assert!(cfg.password.is_none());
        assert!(cfg.tls.is_none());
        assert_eq!(cfg.query_timeout_secs, None);
        assert!(cfg.breaker.is_none());
        assert_eq!(cfg.max_concurrency, None);
        assert_eq!(cfg.local_dc, None);
    }

    async fn config_error(cfg: CassandraConfig) -> RdbmsError {
        match CassandraClient::from_config(cfg).await {
            Ok(_) => panic!("expected configuration error"),
            Err(error) => error,
        }
    }

    #[tokio::test]
    async fn from_config_rejects_empty_nodes() {
        let cfg: CassandraConfig = serde_json::from_str(r#"{"nodes":[],"keyspace":"ks"}"#).unwrap();
        let error = config_error(cfg).await;
        assert!(matches!(error, RdbmsError::Config(_)), "got: {error}");
        assert!(error.to_string().contains("nodes"));
    }

    #[tokio::test]
    async fn from_config_rejects_empty_keyspace() {
        let cfg: CassandraConfig =
            serde_json::from_str(r#"{"nodes":["127.0.0.1:9042"],"keyspace":""}"#).unwrap();
        let error = config_error(cfg).await;
        assert!(matches!(error, RdbmsError::Config(_)), "got: {error}");
        assert!(error.to_string().contains("keyspace"));
    }

    #[tokio::test]
    async fn from_config_rejects_partial_credentials() {
        let cfg: CassandraConfig =
            serde_json::from_str(r#"{"nodes":["127.0.0.1:9042"],"keyspace":"ks","username":"u"}"#)
                .unwrap();
        let error = config_error(cfg).await;
        assert!(matches!(error, RdbmsError::Config(_)), "got: {error}");
        assert!(error.to_string().contains("username and password"));
    }

    #[tokio::test]
    async fn from_config_rejects_partial_client_cert() {
        // Fails during TLS assembly, before any connection attempt.
        let cfg: CassandraConfig = serde_json::from_str(
            r#"{"nodes":["127.0.0.1:9042"],"keyspace":"ks",
                "tls":{"client_cert":"/nonexistent/cert.pem"}}"#,
        )
        .unwrap();
        let error = config_error(cfg).await;
        assert!(matches!(error, RdbmsError::Config(_)), "got: {error}");
        assert!(error.to_string().contains("client_cert and client_key"));
    }
}

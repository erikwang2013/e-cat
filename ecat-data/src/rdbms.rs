// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct Row {
    columns: Vec<String>,
    values: Vec<serde_json::Value>,
}

impl Row {
    /// Create a new Row with the given columns and values.
    pub fn new(columns: Vec<String>, values: Vec<serde_json::Value>) -> Self {
        debug_assert_eq!(
            columns.len(),
            values.len(),
            "columns and values must have the same length"
        );
        Self { columns, values }
    }

    pub fn get(&self, col: &str) -> Option<&serde_json::Value> {
        self.columns
            .iter()
            .position(|c| c == col)
            .and_then(|i| self.values.get(i))
    }
}

/// Inner transaction trait for cross-backend transaction support.
#[async_trait]
pub trait TransactionInner: Send {
    async fn commit(&mut self) -> Result<(), RdbmsError>;
    async fn rollback(&mut self) -> Result<(), RdbmsError>;
}

#[derive(Default)]
pub struct Transaction {
    committed: bool,
    inner: Option<Box<dyn TransactionInner>>,
}

impl Transaction {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_inner(inner: Box<dyn TransactionInner>) -> Self {
        Self {
            inner: Some(inner),
            committed: false,
        }
    }

    pub async fn commit(mut self) -> Result<(), RdbmsError> {
        if let Some(ref mut inner) = self.inner {
            inner.commit().await?;
        }
        self.committed = true;
        Ok(())
    }

    pub async fn rollback(mut self) -> Result<(), RdbmsError> {
        if let Some(ref mut inner) = self.inner {
            inner.rollback().await?;
        }
        self.committed = false;
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // This Drop impl only logs. No SQL is sent here (async work is not
        // possible in Drop); actual rollback relies on the backing sqlx
        // Transaction dropping without commit, which rolls back the
        // underlying DB connection.
        if !self.committed {
            tracing::warn!("transaction dropped without commit — rolling back");
        }
    }
}

#[async_trait]
pub trait RdbmsClient: Send + Sync {
    /// Execute a raw SQL statement. Prefer `execute_with` for user-supplied values.
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError>;
    /// Query rows with raw SQL. Prefer `query_with` for user-supplied values.
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    /// Execute a parameterized SQL statement to prevent injection.
    /// Backends that cannot bind parameters return an error.
    async fn execute_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized execute not supported by this backend".into(),
        ))
    }
    /// Query with parameterized SQL to prevent injection.
    /// Backends that cannot bind parameters return an error.
    async fn query_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized query not supported by this backend".into(),
        ))
    }
    async fn transaction(&self) -> Result<Transaction, RdbmsError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RdbmsError {
    #[error("database error: {0}")]
    Database(String),
    #[error("connection error: {0}")]
    Connection(String),
    #[error("configuration error: {0}")]
    Config(String),
}

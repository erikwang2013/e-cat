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
        Self { columns, values }
    }

    pub fn get(&self, col: &str) -> Option<&serde_json::Value> {
        self.columns
            .iter()
            .position(|c| c == col)
            .and_then(|i| self.values.get(i))
    }
}

#[derive(Default)]
pub struct Transaction {
    committed: bool,
}

impl Transaction {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn commit(mut self) -> Result<(), RdbmsError> {
        self.committed = true;
        Ok(())
    }

    pub fn rollback(mut self) -> Result<(), RdbmsError> {
        self.committed = false;
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.committed {
            // auto-rollback uncommitted transactions
        }
    }
}

#[async_trait]
pub trait RdbmsClient: Send + Sync {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError>;
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    async fn transaction(&self) -> Result<Transaction, RdbmsError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RdbmsError {
    #[error("database error: {0}")]
    Database(String),
    #[error("connection error: {0}")]
    Connection(String),
}

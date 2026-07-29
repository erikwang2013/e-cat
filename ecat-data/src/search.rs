// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;

#[async_trait]
pub trait SearchClient: Send + Sync {
    async fn index(
        &self,
        index: &str,
        id: &str,
        doc: &serde_json::Value,
    ) -> Result<(), SearchError>;
    async fn search(
        &self,
        index: &str,
        query: &serde_json::Value,
    ) -> Result<serde_json::Value, SearchError>;
    async fn delete(&self, index: &str, id: &str) -> Result<(), SearchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search error: {0}")]
    Other(String),
}

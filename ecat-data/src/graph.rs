use async_trait::async_trait;

#[async_trait]
pub trait GraphClient: Send + Sync {
    async fn execute(
        &self,
        query: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, GraphError>;
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("graph error: {0}")]
    Other(String),
}

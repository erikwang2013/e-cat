// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{GraphClient, GraphError};
pub struct NebulaGraphClient {
    client: reqwest::Client,
    base_url: String,
    space: String,
}

impl NebulaGraphClient {
    pub fn new(base_url: impl Into<String>, space: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            space: space.into(),
        }
    }
}

#[async_trait]
impl GraphClient for NebulaGraphClient {
    async fn execute(
        &self,
        ngql: &str,
        _params: &serde_json::Value,
    ) -> Result<serde_json::Value, GraphError> {
        let resp = self
            .client
            .post(format!("{}/api/ngql/execute", self.base_url))
            .json(&serde_json::json!({"gql": ngql, "space": self.space}))
            .send()
            .await
            .map_err(|e| GraphError::Other(format!("nebula: {e}")))?;
        if !resp.status().is_success() {
            return Err(GraphError::Other(resp.text().await.unwrap_or_default()));
        }
        resp.json()
            .await
            .map_err(|e| GraphError::Other(format!("nebula parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_constructs() {
        let _client = NebulaGraphClient::new("http://localhost:19669", "test_space");
    }
}

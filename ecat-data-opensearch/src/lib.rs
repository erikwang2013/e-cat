// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{SearchClient, SearchError};

pub struct OpenSearchClient {
    client: reqwest::Client,
    base_url: String,
}

impl OpenSearchClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }
}

#[async_trait]
impl SearchClient for OpenSearchClient {
    async fn index(
        &self,
        index: &str,
        id: &str,
        doc: &serde_json::Value,
    ) -> Result<(), SearchError> {
        let resp = self
            .client
            .put(format!("{}/{index}/_doc/{id}", self.base_url))
            .json(doc)
            .send()
            .await
            .map_err(|e| SearchError::Other(format!("index: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Other(format!(
                "index failed: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn search(
        &self,
        index: &str,
        query: &serde_json::Value,
    ) -> Result<serde_json::Value, SearchError> {
        self.client
            .post(format!("{}/{index}/_search", self.base_url))
            .json(query)
            .send()
            .await
            .map_err(|e| SearchError::Other(format!("search: {e}")))?
            .json()
            .await
            .map_err(|e| SearchError::Other(format!("parse: {e}")))
    }

    async fn delete(&self, index: &str, id: &str) -> Result<(), SearchError> {
        let resp = self
            .client
            .delete(format!("{}/{index}/_doc/{id}", self.base_url))
            .send()
            .await
            .map_err(|e| SearchError::Other(format!("delete: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Other(format!(
                "delete failed: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs() {
        let _client = OpenSearchClient::new("http://localhost:9200");
    }
}

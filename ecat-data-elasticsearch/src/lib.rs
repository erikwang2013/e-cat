// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{SearchClient, SearchError};

pub struct ElasticsearchClient {
    client: reqwest::Client,
    base_url: String,
}

impl ElasticsearchClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }
}

#[async_trait]
impl SearchClient for ElasticsearchClient {
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
            .map_err(|e| SearchError::Other(format!("es index: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Other(resp.text().await.unwrap_or_default()));
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
            .map_err(|e| SearchError::Other(format!("es search: {e}")))?
            .json()
            .await
            .map_err(|e| SearchError::Other(format!("es parse: {e}")))
    }

    async fn delete(&self, index: &str, id: &str) -> Result<(), SearchError> {
        self.client
            .delete(format!("{}/{index}/_doc/{id}", self.base_url))
            .send()
            .await
            .map_err(|e| SearchError::Other(format!("es delete: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs() {
        let _client = ElasticsearchClient::new("http://localhost:9200");
    }
}

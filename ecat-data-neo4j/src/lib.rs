// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{GraphClient, GraphError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Neo4jConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct Neo4jClient {
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
}

impl Neo4jClient {
    pub fn new(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            username: username.into(),
            password: password.into(),
        }
    }

    pub fn from_config(cfg: Neo4jConfig) -> Result<Self, GraphError> {
        let client = match &cfg.tls {
            Some(tls) if tls.is_enabled() => tls
                .build_reqwest_client()
                .map_err(|e| GraphError::Other(format!("TLS: {e}")))?,
            _ => reqwest::Client::new(),
        };
        Ok(Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
        })
    }
}

#[async_trait]
impl GraphClient for Neo4jClient {
    async fn execute(
        &self,
        cypher: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, GraphError> {
        let body = serde_json::json!({"statements": [{"statement": cypher, "parameters": params}]});
        let resp = self
            .client
            .post(format!("{}/db/data/transaction/commit", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(|e| GraphError::Other(format!("neo4j: {e}")))?;
        if !resp.status().is_success() {
            return Err(GraphError::Other(resp.text().await.unwrap_or_default()));
        }
        resp.json()
            .await
            .map_err(|e| GraphError::Other(format!("neo4j parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_constructs() {
        let _client = Neo4jClient::new("http://localhost:7474", "neo4j", "password");
    }
}

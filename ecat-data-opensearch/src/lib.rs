// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{SearchClient, SearchError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct OpenSearchConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct OpenSearchClient {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
}

impl OpenSearchClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            username: None,
            password: None,
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            username: Some(username.into()),
            password: Some(password.into()),
        }
    }

    pub fn from_config(cfg: OpenSearchConfig) -> Self {
        let client = match &cfg.tls {
            Some(tls) if tls.is_enabled() => tls
                .build_reqwest_client()
                .expect("TLS client build failed"),
            _ => reqwest::Client::new(),
        };
        Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.username, &self.password) {
            (Some(u), Some(p)) => req.basic_auth(u, Some(p)),
            _ => req,
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
        let req = self
            .client
            .put(format!("{}/{index}/_doc/{id}", self.base_url))
            .json(doc);
        let resp = self.apply_auth(req).send().await
            .map_err(|e| SearchError::Other(format!("index: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Other(format!("index failed: {}", resp.text().await.unwrap_or_default())));
        }
        Ok(())
    }

    async fn search(
        &self,
        index: &str,
        query: &serde_json::Value,
    ) -> Result<serde_json::Value, SearchError> {
        let req = self
            .client
            .post(format!("{}/{index}/_search", self.base_url))
            .json(query);
        self.apply_auth(req).send().await
            .map_err(|e| SearchError::Other(format!("search: {e}")))?
            .json()
            .await
            .map_err(|e| SearchError::Other(format!("parse: {e}")))
    }

    async fn delete(&self, index: &str, id: &str) -> Result<(), SearchError> {
        let req = self
            .client
            .delete(format!("{}/{index}/_doc/{id}", self.base_url));
        let resp = self.apply_auth(req).send().await
            .map_err(|e| SearchError::Other(format!("delete: {e}")))?;
        if !resp.status().is_success() {
            return Err(SearchError::Other(format!("delete failed: {}", resp.text().await.unwrap_or_default())));
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

    #[test]
    fn client_with_auth() {
        let _client = OpenSearchClient::with_auth("http://localhost:9200", "admin", "secret");
    }

    #[test]
    fn config_with_optional_auth() {
        let cfg: OpenSearchConfig = serde_json::from_str(
            r#"{"base_url":"http://localhost:9200","username":"admin","password":"secret"}"#
        ).unwrap();
        let client = OpenSearchClient::from_config(cfg);
        assert!(client.username.is_some());
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! QuestDB client (HTTP `/exec` endpoint).
//!
//! Error responses pass through the server's raw body text: credentials are
//! sent via the Authorization header (never in the URL), so error messages
//! cannot leak secrets; outer layers handle the generic error text.

use async_trait::async_trait;
use ecat_data::{RdbmsClient, RdbmsError, Row};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct QuestdbConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct QuestdbClient {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
}

impl QuestdbClient {
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

    pub fn from_config(cfg: QuestdbConfig) -> Result<Self, RdbmsError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| RdbmsError::Config(format!("TLS: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
        })
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        ecat_tls::apply_basic_auth(req, &self.username, &self.password)
    }
}

#[async_trait]
impl RdbmsClient for QuestdbClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let req = self
            .client
            .post(format!("{}/exec", self.base_url))
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(sql.to_string());
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("questdb: {e}")))?;
        if !resp.status().is_success() {
            return Err(RdbmsError::Database(
                resp.text()
                    .await
                    .unwrap_or_else(|e| format!("questdb: {e}")),
            ));
        }
        Ok(0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let req = self
            .client
            .post(format!("{}/exec?count=true", self.base_url))
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("Accept", "application/json")
            .body(sql.to_string());
        let body: serde_json::Value = self
            .apply_auth(req)
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("questdb: {e}")))?
            .json()
            .await
            .map_err(|e| RdbmsError::Database(format!("questdb parse: {e}")))?;
        let mut rows = Vec::new();
        if let Some(columns) = body.get("columns").and_then(|c| c.as_array()) {
            let cols: Vec<String> = columns
                .iter()
                .filter_map(|c| {
                    c.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            if let Some(dataset) = body.get("dataset").and_then(|d| d.as_array()) {
                for row in dataset {
                    if let Some(vals) = row.as_array() {
                        rows.push(Row::new(cols.clone(), vals.clone()));
                    }
                }
            }
        }
        Ok(rows)
    }

    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "QuestDB does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs() {
        let _client = QuestdbClient::new("http://localhost:9000");
    }

    #[test]
    fn config_with_optional_auth() {
        let cfg: QuestdbConfig = serde_json::from_str(
            r#"{"base_url":"http://localhost:9000","username":"admin","password":"quest"}"#,
        )
        .unwrap();
        let client = QuestdbClient::from_config(cfg).unwrap();
        assert!(client.username.is_some());
    }
}

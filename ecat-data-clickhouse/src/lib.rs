// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{RdbmsClient, RdbmsError, Row};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ClickhouseConfig {
    pub base_url: String,
    #[serde(default = "default_database")]
    pub database: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

fn default_database() -> String {
    "default".into()
}

pub struct ClickhouseClient {
    client: reqwest::Client,
    base_url: String,
    database: String,
    username: Option<String>,
    password: Option<String>,
}

impl ClickhouseClient {
    pub fn new(base_url: impl Into<String>, database: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            database: database.into(),
            username: None,
            password: None,
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        database: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            database: database.into(),
            username: Some(username.into()),
            password: Some(password.into()),
        }
    }

    pub fn from_config(cfg: ClickhouseConfig) -> Self {
        let client = match &cfg.tls {
            Some(tls) if tls.is_enabled() => tls
                .build_reqwest_client()
                .expect("TLS client build failed"),
            _ => reqwest::Client::new(),
        };
        Self {
            client,
            base_url: cfg.base_url,
            database: cfg.database,
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
impl RdbmsClient for ClickhouseClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let req = self
            .client
            .post(&self.base_url)
            .query(&[("database", &self.database)])
            .body(sql.to_string());
        let resp = self.apply_auth(req).send().await
            .map_err(|e| RdbmsError::Database(format!("ch: {e}")))?;
        if !resp.status().is_success() {
            return Err(RdbmsError::Database(resp.text().await.unwrap_or_default()));
        }
        Ok(0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let req = self
            .client
            .post(&self.base_url)
            .query(&[
                ("database", &self.database),
                ("default_format", &"JSONEachRow".to_string()),
            ])
            .body(sql.to_string());
        let resp = self.apply_auth(req).send().await
            .map_err(|e| RdbmsError::Database(format!("ch query: {e}")))?;
        let text = resp.text().await
            .map_err(|e| RdbmsError::Database(format!("ch read: {e}")))?;
        let mut rows = Vec::new();
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                && let Some(obj) = v.as_object()
            {
                let cols: Vec<String> = obj.keys().cloned().collect();
                let vals: Vec<serde_json::Value> = obj.values().cloned().collect();
                rows.push(Row::new(cols, vals));
            }
        }
        Ok(rows)
    }

    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "ClickHouse does not support transactions".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs() {
        let _client = ClickhouseClient::new("http://localhost:8123", "default");
    }

    #[test]
    fn config_with_optional_auth() {
        let cfg: ClickhouseConfig = serde_json::from_str(
            r#"{"base_url":"http://localhost:8123","username":"default","password":"secret"}"#
        ).unwrap();
        let client = ClickhouseClient::from_config(cfg);
        assert!(client.username.is_some());
    }
}

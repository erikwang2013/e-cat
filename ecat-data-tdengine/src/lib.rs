// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{DataPoint, FieldValue, TsdbClient, TsdbError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct TdengineConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct TdengineClient {
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
    database: Option<String>,
}

impl TdengineClient {
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
            database: None,
        }
    }

    pub fn from_config(cfg: TdengineConfig) -> Result<Self, TsdbError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| TsdbError::Other(format!("TLS: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
            database: cfg.database,
        })
    }

    fn sql_url(&self) -> String {
        match &self.database {
            Some(db) => format!("{}/rest/sql/{}", self.base_url, db),
            None => format!("{}/rest/sql", self.base_url),
        }
    }

    async fn exec(&self, sql: &str) -> Result<serde_json::Value, TsdbError> {
        let resp = self
            .client
            .post(self.sql_url())
            .basic_auth(&self.username, Some(&self.password))
            .json(&serde_json::json!({ "sql": sql }))
            .send()
            .await
            .map_err(|e| TsdbError::Other(format!("tdengine exec: {e}")))?;
        if !resp.status().is_success() {
            return Err(TsdbError::Other(resp.text().await.unwrap_or_default()));
        }
        resp.json()
            .await
            .map_err(|e| TsdbError::Other(format!("tdengine parse: {e}")))
    }
}

#[async_trait]
impl TsdbClient for TdengineClient {
    async fn write(&self, points: &[DataPoint]) -> Result<(), TsdbError> {
        for p in points {
            // Tags are flattened as columns; measurement is the table name.
            let mut cols = Vec::new();
            let mut vals = Vec::new();
            cols.push("ts".to_string());
            vals.push(
                p.timestamp
                    .map(|ts| ts.to_string())
                    .unwrap_or_else(|| "now".to_string()),
            );
            for (k, v) in &p.tags {
                cols.push(format!("\"{k}\""));
                vals.push(format!("\"{v}\""));
            }
            for (k, v) in &p.fields {
                cols.push(format!("\"{k}\""));
                vals.push(match v {
                    FieldValue::Float(f) => f.to_string(),
                    FieldValue::Int(i) => i.to_string(),
                    FieldValue::String(s) => format!("\"{s}\""),
                    FieldValue::Bool(b) => {
                        if *b {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        }
                    }
                });
            }
            let sql = format!(
                "INSERT INTO \"{}\" ({}) VALUES ({})",
                p.measurement,
                cols.join(", "),
                vals.join(", ")
            );
            self.exec(&sql).await?;
        }
        Ok(())
    }

    async fn query(&self, sql: &str) -> Result<serde_json::Value, TsdbError> {
        self.exec(sql).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_deserializes() {
        let cfg: TdengineConfig = serde_json::from_value(serde_json::json!({
            "base_url": "http://localhost:6041",
            "username": "root",
            "password": "taosdata",
            "database": "demo",
        }))
        .unwrap();
        assert_eq!(cfg.database.as_deref(), Some("demo"));
        assert!(cfg.tls.is_none());
    }

    #[test]
    fn client_constructs() {
        let _client = TdengineClient::new("http://localhost:6041", "root", "taosdata");
    }
}

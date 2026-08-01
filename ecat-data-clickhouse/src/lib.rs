// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{Row, RdbmsClient, RdbmsError};

pub struct ClickhouseClient {
    client: reqwest::Client,
    base_url: String,
    database: String,
}

impl ClickhouseClient {
    pub fn new(base_url: impl Into<String>, database: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            database: database.into(),
        }
    }
}

#[async_trait]
impl RdbmsClient for ClickhouseClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let resp = self
            .client
            .post(&self.base_url)
            .query(&[("database", &self.database)])
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("ch: {e}")))?;
        if !resp.status().is_success() {
            return Err(RdbmsError::Database(resp.text().await.unwrap_or_default()));
        }
        Ok(0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let resp = self
            .client
            .post(&self.base_url)
            .query(&[
                ("database", &self.database),
                ("default_format", &"JSONEachRow".to_string()),
            ])
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("ch query: {e}")))?;
        let text = resp
            .text()
            .await
            .map_err(|e| RdbmsError::Database(format!("ch read: {e}")))?;
        let mut rows = Vec::new();
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(obj) = v.as_object() {
                    let cols: Vec<String> = obj.keys().cloned().collect();
                    let vals: Vec<serde_json::Value> = obj.values().cloned().collect();
                    rows.push(Row::new(cols, vals));
                }
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
}

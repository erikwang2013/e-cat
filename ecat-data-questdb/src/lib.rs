// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{RdbmsClient, RdbmsError, Row};

pub struct QuestdbClient {
    client: reqwest::Client,
    base_url: String,
}

impl QuestdbClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }
}

#[async_trait]
impl RdbmsClient for QuestdbClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let resp = self
            .client
            .get(format!("{}/exec", self.base_url))
            .query(&[("query", sql)])
            .send()
            .await
            .map_err(|e| RdbmsError::Database(format!("questdb: {e}")))?;
        if !resp.status().is_success() {
            return Err(RdbmsError::Database(
                resp.text().await.unwrap_or_else(|e| format!("questdb: {e}")),
            ));
        }
        Ok(0)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let body: serde_json::Value = self
            .client
            .get(format!("{}/exec", self.base_url))
            .query(&[("query", sql), ("count", "true")])
            .header("Accept", "application/json")
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
}

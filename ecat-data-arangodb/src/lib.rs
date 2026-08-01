// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{GraphClient, GraphError};
pub struct ArangoClient {
    client: reqwest::Client,
    base_url: String,
    db: String,
    username: String,
    password: String,
}

impl ArangoClient {
    pub fn new(
        base_url: impl Into<String>,
        db: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            db: db.into(),
            username: username.into(),
            password: password.into(),
        }
    }
}

#[async_trait]
impl GraphClient for ArangoClient {
    async fn execute(
        &self,
        aql: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, GraphError> {
        let body = serde_json::json!({"query": aql, "bindVars": params});
        let resp = self
            .client
            .post(format!("{}/_db/{}/_api/cursor", self.base_url, self.db))
            .basic_auth(&self.username, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(|e| GraphError::Other(format!("arango: {e}")))?;
        if !resp.status().is_success() {
            return Err(GraphError::Other(resp.text().await.unwrap_or_default()));
        }
        resp.json()
            .await
            .map_err(|e| GraphError::Other(format!("arango parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_constructs() {
        let _client = ArangoClient::new("http://localhost:8529", "mydb", "root", "");
    }
}

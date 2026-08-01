// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{GraphClient, GraphError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct NebulaGraphConfig {
    pub base_url: String,
    pub space: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct NebulaGraphClient {
    client: reqwest::Client,
    base_url: String,
    space: String,
    username: Option<String>,
    password: Option<String>,
}

impl NebulaGraphClient {
    pub fn new(base_url: impl Into<String>, space: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            space: space.into(),
            username: None,
            password: None,
        }
    }

    pub fn with_auth(
        base_url: impl Into<String>,
        space: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            space: space.into(),
            username: Some(username.into()),
            password: Some(password.into()),
        }
    }

    pub fn from_config(cfg: NebulaGraphConfig) -> Self {
        let client = match &cfg.tls {
            Some(tls) if tls.is_enabled() => tls
                .build_reqwest_client()
                .expect("TLS client build failed"),
            _ => reqwest::Client::new(),
        };
        Self {
            client,
            base_url: cfg.base_url,
            space: cfg.space,
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
impl GraphClient for NebulaGraphClient {
    async fn execute(
        &self,
        ngql: &str,
        _params: &serde_json::Value,
    ) -> Result<serde_json::Value, GraphError> {
        let req = self
            .client
            .post(format!("{}/api/ngql/execute", self.base_url))
            .json(&serde_json::json!({"gql": ngql, "space": self.space}));
        let resp = self.apply_auth(req).send().await
            .map_err(|e| GraphError::Other(format!("nebula: {e}")))?;
        if !resp.status().is_success() {
            return Err(GraphError::Other(resp.text().await.unwrap_or_default()));
        }
        resp.json().await
            .map_err(|e| GraphError::Other(format!("nebula parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs() {
        let _client = NebulaGraphClient::new("http://localhost:19669", "test_space");
    }

    #[test]
    fn config_with_optional_auth() {
        let cfg: NebulaGraphConfig = serde_json::from_str(
            r#"{"base_url":"http://localhost:19669","space":"test","username":"root","password":"nebula"}"#
        ).unwrap();
        let client = NebulaGraphClient::from_config(cfg);
        assert!(client.username.is_some());
    }
}

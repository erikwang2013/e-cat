// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{DataPoint, FieldValue, TsdbClient, TsdbError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct IotdbConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct IotdbClient {
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
}

impl IotdbClient {
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

    pub fn from_config(cfg: IotdbConfig) -> Self {
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
}

#[async_trait]
impl TsdbClient for IotdbClient {
    async fn write(&self, points: &[DataPoint]) -> Result<(), TsdbError> {
        for p in points {
            let mut body =
                serde_json::json!({"measurement": p.measurement, "tags": p.tags, "fields": {}});
            if let Some(ts) = p.timestamp {
                body["timestamp"] = serde_json::Value::Number(ts.into());
            }
            for (k, v) in &p.fields {
                body["fields"][k] = match v {
                    FieldValue::Float(f) => serde_json::Value::Number(
                        serde_json::Number::from_f64(*f).unwrap_or(0.into()),
                    ),
                    FieldValue::Int(i) => serde_json::Value::Number((*i).into()),
                    FieldValue::String(s) => serde_json::Value::String(s.clone()),
                    FieldValue::Bool(b) => serde_json::Value::Bool(*b),
                };
            }
            let resp = self
                .client
                .post(format!("{}/rest/v2/insertTablet", self.base_url))
                .basic_auth(&self.username, Some(&self.password))
                .json(&body)
                .send()
                .await
                .map_err(|e| TsdbError::Other(format!("iotdb write: {e}")))?;
            if !resp.status().is_success() {
                return Err(TsdbError::Other(resp.text().await.unwrap_or_default()));
            }
        }
        Ok(())
    }

    async fn query(&self, sql: &str) -> Result<serde_json::Value, TsdbError> {
        let resp = self
            .client
            .post(format!("{}/rest/v2/query", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| TsdbError::Other(format!("iotdb query: {e}")))?;
        if !resp.status().is_success() {
            return Err(TsdbError::Other(resp.text().await.unwrap_or_default()));
        }
        resp.json()
            .await
            .map_err(|e| TsdbError::Other(format!("iotdb parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_constructs() {
        let _client = IotdbClient::new("http://localhost:18080", "root", "root");
    }
}

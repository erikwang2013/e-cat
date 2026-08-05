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

    pub fn from_config(cfg: IotdbConfig) -> Result<Self, TsdbError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| TsdbError::Other(format!("TLS: {e}")))?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
            username: cfg.username,
            password: cfg.password,
        })
    }
}

#[async_trait]
impl TsdbClient for IotdbClient {
    async fn write(&self, points: &[DataPoint]) -> Result<(), TsdbError> {
        for p in points {
            // Apache IoTDB REST v2 insertTablet body:
            // {"device": "...", "is_aligned": false, "timestamps": [...],
            //  "measurements": [...], "data_types": [...], "values": [[...]]}
            // `device` = measurement; tags are not representable in this API.
            let mut measurements = Vec::with_capacity(p.fields.len());
            let mut data_types = Vec::with_capacity(p.fields.len());
            let mut values: Vec<serde_json::Value> = Vec::with_capacity(p.fields.len());
            for (k, v) in &p.fields {
                measurements.push(k.clone());
                let (dt, val) = match v {
                    FieldValue::Float(f) => (
                        "DOUBLE",
                        serde_json::Value::Number(
                            serde_json::Number::from_f64(*f).unwrap_or(0.into()),
                        ),
                    ),
                    FieldValue::Int(i) => ("INT64", serde_json::Value::Number((*i).into())),
                    FieldValue::String(s) => ("TEXT", serde_json::Value::String(s.clone())),
                    FieldValue::Bool(b) => ("BOOLEAN", serde_json::Value::Bool(*b)),
                };
                data_types.push(dt);
                values.push(val);
            }
            let body = serde_json::json!({
                "device": p.measurement,
                "is_aligned": false,
                "timestamps": [p.timestamp.unwrap_or(0)],
                "measurements": measurements,
                "data_types": data_types,
                "values": [values],
            });
            let resp = self
                .client
                .post(format!("{}/rest/v2/insertTablet", self.base_url))
                .basic_auth(&self.username, Some(&self.password))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| TsdbError::Other(format!("iotdb write: {e}")))?;
            if !resp.status().is_success() {
                return Err(TsdbError::Other(resp.text().await.unwrap_or_default()));
            }
            // IoTDB REST v2 may return HTTP 200 with a body `code` != 200 on
            // some failures; surface those too.
            if let Ok(v) = resp.json::<serde_json::Value>().await
                && let Some(code) = v.get("code").and_then(|c| c.as_i64())
                && code != 200
            {
                return Err(TsdbError::Other(format!(
                    "iotdb write failed: code {code}: {}",
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("no message")
                )));
            }
        }
        Ok(())
    }

    async fn query(&self, sql: &str) -> Result<serde_json::Value, TsdbError> {
        let resp = self
            .client
            .post(format!("{}/rest/v2/query", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "text/plain; charset=utf-8")
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

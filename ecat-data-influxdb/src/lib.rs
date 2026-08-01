// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{DataPoint, FieldValue, TsdbClient, TsdbError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct InfluxConfig {
    pub base_url: String,
    pub org: String,
    pub bucket: String,
    pub token: String,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct InfluxClient {
    client: reqwest::Client,
    write_url: String,
    query_url: String,
    org: String,
    bucket: String,
    token: String,
}

impl InfluxClient {
    pub fn new(
        base_url: impl Into<String>,
        org: impl Into<String>,
        bucket: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        let base = base_url.into();
        Self {
            write_url: format!("{base}/api/v2/write"),
            query_url: format!("{base}/api/v2/query"),
            org: org.into(),
            bucket: bucket.into(),
            token: token.into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn from_config(cfg: InfluxConfig) -> Self {
        let base = cfg.base_url.clone();
        let client = match &cfg.tls {
            Some(tls) if tls.is_enabled() => tls
                .build_reqwest_client()
                .expect("TLS client build failed"),
            _ => reqwest::Client::new(),
        };
        Self {
            write_url: format!("{base}/api/v2/write"),
            query_url: format!("{base}/api/v2/query"),
            org: cfg.org,
            bucket: cfg.bucket,
            token: cfg.token,
            client,
        }
    }
}

#[async_trait]
impl TsdbClient for InfluxClient {
    async fn write(&self, points: &[DataPoint]) -> Result<(), TsdbError> {
        let mut lines = String::new();
        for p in points {
            let tags: String = if p.tags.is_empty() {
                String::new()
            } else {
                p.tags
                    .iter()
                    .map(|(k, v)| format!(",{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("")
            };
            let fields: String = p
                .fields
                .iter()
                .map(|(k, v)| match v {
                    FieldValue::Float(f) => format!("{k}={f}"),
                    FieldValue::Int(i) => format!("{k}={i}i"),
                    FieldValue::String(s) => format!("{k}=\"{s}\""),
                    FieldValue::Bool(b) => format!("{k}={b}"),
                })
                .collect::<Vec<_>>()
                .join(",");
            lines.push_str(&format!("{}{tags} {fields}", p.measurement));
            if let Some(ts) = p.timestamp {
                lines.push_str(&format!(" {ts}"));
            }
            lines.push('\n');
        }

        let resp = self
            .client
            .post(&self.write_url)
            .header("Authorization", format!("Token {}", self.token))
            .query(&[
                ("org", &self.org),
                ("bucket", &self.bucket),
                ("precision", &"ns".to_string()),
            ])
            .body(lines)
            .send()
            .await
            .map_err(|e| TsdbError::Other(format!("write: {e}")))?;

        if !resp.status().is_success() {
            return Err(TsdbError::Other(format!(
                "write failed: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn query(&self, query: &str) -> Result<serde_json::Value, TsdbError> {
        self.client
            .post(&self.query_url)
            .header("Authorization", format!("Token {}", self.token))
            .header("Content-Type", "application/vnd.flux")
            .query(&[("org", &self.org)])
            .body(query.to_string())
            .send()
            .await
            .map_err(|e| TsdbError::Other(format!("query: {e}")))?
            .json()
            .await
            .map_err(|e| TsdbError::Other(format!("parse: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs() {
        let _client = InfluxClient::new("http://localhost:8086", "myorg", "mybucket", "mytoken");
    }

    #[test]
    fn data_point_builder() {
        let p = DataPoint::new("cpu")
            .with_tag("host", "server01")
            .with_field("usage", FieldValue::Float(0.85))
            .with_timestamp(1625097600000000000);
        assert_eq!(p.measurement, "cpu");
        assert_eq!(p.tags.get("host").unwrap(), "server01");
    }
}

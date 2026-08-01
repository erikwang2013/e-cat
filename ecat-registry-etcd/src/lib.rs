// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_registry::{Registration, Registry, RegistryError, ServiceInfo};
use std::sync::Arc;

#[derive(Clone)]
pub struct EtcdRegistry {
    client: reqwest::Client,
    endpoints: Vec<String>,
    prefix: String,
    lease_ttl: u64,
}

impl EtcdRegistry {
    pub fn new(endpoints: Vec<String>, prefix: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoints,
            prefix: prefix.into(),
            lease_ttl: 30,
        }
    }

    pub fn lease_ttl(mut self, ttl: u64) -> Self {
        self.lease_ttl = ttl;
        self
    }

    fn base_url(&self) -> &str {
        self.endpoints
            .first()
            .map(|s| s.as_str())
            .unwrap_or("http://127.0.0.1:2379")
    }
}

#[async_trait]
impl Registry for EtcdRegistry {
    async fn register(&self, service: ServiceInfo) -> Result<Registration, RegistryError> {
        let id = format!("{}/{}", self.prefix, service.name);
        let lease_id = create_lease(&self.client, self.base_url(), self.lease_ttl)
            .await
            .map_err(RegistryError::Other)?;
        let key = format!(
            "/ecat/services/{}/{}/{}",
            self.prefix,
            service.name,
            uuid::Uuid::new_v4()
        );
        let value = serde_json::to_string(&service)
            .map_err(|e| RegistryError::Other(format!("serialize: {e}")))?;
        let body = serde_json::json!({
            "key": b64(&key),
            "value": b64(&value),
            "lease": lease_id.to_string(),
        });
        self.client
            .post(format!("{}/v3/kv/put", self.base_url()))
            .json(&body)
            .send()
            .await
            .map_err(|e| RegistryError::Other(format!("etcd put: {e}")))?;
        Ok(Registration::new(id, service, Arc::new(self.clone())))
    }

    async fn deregister(&self, id: &str) -> Result<(), RegistryError> {
        self.client
            .post(format!("{}/v3/kv/deleterange", self.base_url()))
            .json(&serde_json::json!({"key": b64(id)}))
            .send()
            .await
            .map_err(|e| RegistryError::Other(format!("etcd del: {e}")))?;
        Ok(())
    }

    async fn discover(&self, name: &str) -> Result<Vec<ServiceInfo>, RegistryError> {
        let prefix = format!("/ecat/services/{}/{}/", self.prefix, name);
        let resp = self
            .client
            .post(format!("{}/v3/kv/range", self.base_url()))
            .json(&serde_json::json!({"key": b64(&prefix), "range_end": b64(&prefix_end(&prefix))}))
            .send()
            .await
            .map_err(|e| RegistryError::Other(format!("etcd range: {e}")))?;
        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| RegistryError::Other(format!("etcd parse: {e}")))?;
        let mut services = Vec::new();
        if let Some(kvs) = result.get("kvs").and_then(|v| v.as_array()) {
            for kv in kvs {
                if let Some(v) = kv.get("value").and_then(|v| v.as_str()) {
                    if let Ok(svc) = decode_b64_str(v).and_then(|s| {
                        serde_json::from_str::<ServiceInfo>(&s).map_err(|e| e.to_string())
                    }) {
                        services.push(svc);
                    }
                }
            }
        }
        Ok(services)
    }

    async fn list_services(&self) -> Result<Vec<String>, RegistryError> {
        let svcs = self.discover("").await?;
        let mut names: Vec<String> = svcs.into_iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
}

async fn create_lease(client: &reqwest::Client, base_url: &str, ttl: u64) -> Result<i64, String> {
    let resp = client
        .post(format!("{base_url}/v3/lease/grant"))
        .json(&serde_json::json!({"TTL": ttl.to_string()}))
        .send()
        .await
        .map_err(|e| format!("etcd lease: {e}"))?;
    let body: serde_json::Value = resp.json().await.map_err(|e| format!("etcd parse: {e}"))?;
    body.get("ID")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "no lease ID".into())
}

fn prefix_end(prefix: &str) -> String {
    let mut bytes = prefix.as_bytes().to_vec();
    for i in (0..bytes.len()).rev() {
        if bytes[i] < 0xff {
            bytes[i] += 1;
            bytes.truncate(i + 1);
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn b64(s: &str) -> String {
    base64_encode(s.as_bytes())
}

fn decode_b64_str(s: &str) -> Result<String, String> {
    let bytes = base64_decode(s)?;
    String::from_utf8(bytes).map_err(|e| format!("utf8: {e}"))
}

fn base64_encode(data: &[u8]) -> String {
    let chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(chars.as_bytes()[(n >> 18) as usize] as char);
        result.push(chars.as_bytes()[((n >> 12) & 0x3f) as usize] as char);
        result.push(if chunk.len() > 1 {
            chars.as_bytes()[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            chars.as_bytes()[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    result
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = std::collections::HashMap::new();
    for (i, c) in chars.chars().enumerate() {
        table.insert(c, i as u8);
    }
    let s = s.trim_end_matches('=');
    let bytes: Vec<u8> = s.chars().filter_map(|c| table.get(&c).copied()).collect();
    let mut result = Vec::new();
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        result.push((chunk[0] << 2) | (chunk[1] >> 4));
        if chunk.len() >= 3 {
            result.push((chunk[1] << 4) | (chunk[2] >> 2));
        }
        if chunk.len() >= 4 {
            result.push((chunk[2] << 6) | chunk[3]);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etcd_registry_constructs() {
        let _reg = EtcdRegistry::new(vec!["http://localhost:2379".into()], "ecat").lease_ttl(60);
    }

    #[test]
    fn b64_roundtrip() {
        let input = "hello-world";
        let encoded = b64(input);
        let decoded = decode_b64_str(&encoded).unwrap();
        assert_eq!(decoded, input);
    }
}

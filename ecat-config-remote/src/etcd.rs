// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! etcd v3 HTTP 配置源（`/v3/kv/range`）。
//!
//! 首版按前缀拉全量 KV；调用方可轮询。认证走 HTTP Basic（etcd auth 开启时），
//! TLS 由 reqwest/rustls 处理。
use async_trait::async_trait;
use base64::Engine as _;
use ecat_config::{ConfigError, ConfigSource};
use std::collections::HashMap;
use std::time::Duration;

/// etcd range 请求超时；range 不是阻塞查询，无需 Consul 那样的长等待。
const REQUEST_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
pub struct EtcdConfigSource {
    client: reqwest::Client,
    endpoints: Vec<String>,
    prefix: String,
    auth: Option<(String, String)>,
}

impl EtcdConfigSource {
    pub fn new(endpoints: Vec<String>, prefix: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoints,
            prefix: prefix.into(),
            auth: None,
        }
    }

    pub fn with_auth(mut self, user: &str, password: &str) -> Self {
        self.auth = Some((user.to_owned(), password.to_owned()));
        self
    }

    /// 依次尝试各 endpoint，返回第一个成功的 KV 前缀快照。
    async fn fetch(&self) -> Result<HashMap<String, serde_json::Value>, ConfigError> {
        if self.endpoints.is_empty() {
            return Err(ConfigError::Other(
                "etcd kv: no endpoints configured".into(),
            ));
        }
        let mut last_error = None;
        for endpoint in &self.endpoints {
            match self.fetch_from(endpoint).await {
                Ok(map) => return Ok(map),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| ConfigError::Other("etcd kv: no endpoints".into())))
    }

    async fn fetch_from(
        &self,
        endpoint: &str,
    ) -> Result<HashMap<String, serde_json::Value>, ConfigError> {
        let url = format!("{}/v3/kv/range", endpoint.trim_end_matches('/'));
        let body = serde_json::json!({
            "key": base64::engine::general_purpose::STANDARD.encode(self.prefix.as_bytes()),
            "range_end": base64::engine::general_purpose::STANDARD
                .encode(prefix_end(self.prefix.as_bytes())),
        });
        let mut builder = self
            .client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));
        if let Some((user, password)) = &self.auth {
            builder = builder.basic_auth(user, Some(password));
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ConfigError::Other(format!("etcd kv {endpoint}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConfigError::Other(format!(
                "etcd kv {endpoint}: {status}: {body}"
            )));
        }
        let response: EtcdRangeResponse = resp
            .json()
            .await
            .map_err(|e| ConfigError::Other(format!("etcd parse {endpoint}: {e}")))?;

        let mut map = HashMap::new();
        for kv in response.kvs {
            let (Some(key), Some(value)) = (decode(&kv.key), decode(&kv.value)) else {
                continue;
            };
            let (Ok(key), Ok(value)) = (String::from_utf8(key), String::from_utf8(value)) else {
                continue;
            };
            let relative = key
                .strip_prefix(&self.prefix)
                .unwrap_or(&key)
                .trim_matches('/')
                .replace('/', ".");
            let value =
                serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value.clone()));
            map.insert(relative, value);
        }
        Ok(map)
    }
}

#[async_trait]
impl ConfigSource for EtcdConfigSource {
    async fn load(&self) -> Result<HashMap<String, serde_json::Value>, ConfigError> {
        self.fetch().await
    }
}

#[derive(Debug, serde::Deserialize)]
struct EtcdRangeResponse {
    #[serde(default)]
    kvs: Vec<EtcdKv>,
}

#[derive(Debug, serde::Deserialize)]
struct EtcdKv {
    key: String,
    value: String,
}

fn decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// etcd 前缀范围的 `range_end`：末字节自增，跳过 0xFF；空前缀表示全量。
fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    let mut bytes = prefix.to_vec();
    while let Some(last) = bytes.last_mut() {
        if *last == u8::MAX {
            bytes.pop();
        } else {
            *last += 1;
            return bytes;
        }
    }
    vec![0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_end_increments_last_byte() {
        assert_eq!(prefix_end(b"app"), b"apq".to_vec());
        assert_eq!(prefix_end(b"app/"), b"app0".to_vec());
    }

    #[test]
    fn prefix_end_skips_overflow_bytes() {
        assert_eq!(prefix_end(b"ap\xff"), b"aq".to_vec());
        assert_eq!(prefix_end(b"a\xff\xff"), b"b".to_vec());
        assert_eq!(prefix_end(b""), vec![0]);
    }

    #[test]
    fn etcd_source_constructs() {
        let _source = EtcdConfigSource::new(vec!["http://localhost:2379".into()], "ecat")
            .with_auth("root", "secret");
    }

    #[test]
    fn decode_rejects_invalid_base64() {
        assert_eq!(decode("aGVsbG8=").unwrap(), b"hello");
        assert!(decode("!!not-base64!!").is_none());
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_config::{ConfigError, ConfigSource};
use std::collections::HashMap;

pub struct ConsulConfigSource {
    client: reqwest::Client,
    base_url: String,
    prefix: String,
    token: Option<String>,
}

impl ConsulConfigSource {
    pub fn new(base_url: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            prefix: prefix.into(),
            token: None,
        }
    }

    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }
}

#[async_trait]
impl ConfigSource for ConsulConfigSource {
    async fn load(&self) -> Result<HashMap<String, serde_json::Value>, ConfigError> {
        let url = format!("{}/v1/kv/{}?recurse=true", self.base_url, self.prefix);
        let mut builder = self.client.get(&url);
        if let Some(ref token) = self.token {
            builder = builder.header("X-Consul-Token", token);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| ConfigError::Other(format!("consul kv: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ConfigError::Other(format!("consul kv failed: {body}")));
        }

        let entries: Vec<ConsulKvEntry> = resp
            .json()
            .await
            .map_err(|e| ConfigError::Other(format!("consul parse: {e}")))?;

        let mut map = HashMap::new();
        for entry in entries {
            let key = entry
                .key
                .strip_prefix(&self.prefix)
                .unwrap_or(&entry.key)
                .trim_matches('/')
                .replace('/', ".");
            if let Some(decoded) = entry.decoded_value() {
                if let Ok(v) = serde_json::from_str(&decoded) {
                    map.insert(key, v);
                } else {
                    map.insert(key, serde_json::Value::String(decoded));
                }
            }
        }

        Ok(map)
    }
}

#[derive(Debug, serde::Deserialize)]
struct ConsulKvEntry {
    #[serde(rename = "Key")]
    key: String,
    #[serde(rename = "Value")]
    value: Option<String>,
}

impl ConsulKvEntry {
    fn decoded_value(&self) -> Option<String> {
        self.value.as_ref().and_then(|v| base64_decode(v).ok().and_then(|bytes| String::from_utf8(bytes).ok()))
    }
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let chars: Vec<char> =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
            .chars()
            .collect();
    let mut table = std::collections::HashMap::new();
    for (i, &c) in chars.iter().enumerate() {
        table.insert(c, i as u8);
    }

    let input = input.trim_end_matches('=');
    let mut result = Vec::new();
    let bytes: Vec<u8> = input
        .chars()
        .filter_map(|c| table.get(&c).copied())
        .collect();

    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let b0 = chunk[0];
        let b1 = chunk[1];
        result.push((b0 << 2) | (b1 >> 4));
        if chunk.len() >= 3 {
            result.push((b1 << 4) | (chunk[2] >> 2));
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
    fn consul_source_constructs() {
        let _src =
            ConsulConfigSource::new("http://consul:8500", "app/config").token("secret");
    }

    #[test]
    fn base64_decode_simple() {
        let result = base64_decode("aGVsbG8=").unwrap();
        assert_eq!(String::from_utf8(result).unwrap(), "hello");
    }
}

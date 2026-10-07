// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_registry::{Registration, Registry, RegistryError, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ConsulRegistry {
    client: reqwest::Client,
    base_url: String,
    datacenter: String,
    token: Option<String>,
}

impl ConsulRegistry {
    /// base_url 必须为 https（本机回环地址允许 http，便于开发）。拒绝
    /// 明文远程地址：Consul API 带 token 时走明文会泄露凭据，构造期
    /// 强制即可保证 token 只出现在受保护连接上。
    pub fn new(base_url: impl Into<String>) -> Result<Self, String> {
        let base_url = base_url.into();
        let url = reqwest::Url::parse(&base_url)
            .map_err(|e| format!("invalid consul base_url '{base_url}': {e}"))?;
        let host = url.host_str().unwrap_or("");
        if url.scheme() != "https" && !is_loopback_host(host) {
            return Err(format!(
                "consul base_url must be https (loopback http allowed for dev), got '{base_url}'"
            ));
        }
        Ok(Self {
            client: reqwest::Client::new(),
            base_url,
            datacenter: "dc1".into(),
            token: None,
        })
    }

    pub fn datacenter(self, dc: impl Into<String>) -> Self {
        Self {
            datacenter: dc.into(),
            ..self
        }
    }

    /// token 只会随请求发送；base_url 已在构造期被强制为 https 或回环
    /// http，凭据不会流经明文远程连接。
    pub fn token(self, token: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
            ..self
        }
    }

    fn headers(&self) -> HashMap<String, String> {
        let mut h = HashMap::new();
        if let Some(ref token) = self.token {
            h.insert("X-Consul-Token".into(), token.clone());
        }
        h
    }
}

#[async_trait]
impl Registry for ConsulRegistry {
    async fn register(&self, service: ServiceInfo) -> Result<Registration, RegistryError> {
        let id = format!("{}-{}", service.name, uuid::Uuid::new_v4());
        let endpoint = service.endpoints.first();
        let address = endpoint
            .map(|e| extract_host(e))
            .unwrap_or("127.0.0.1")
            .to_string();
        let port = endpoint.and_then(|e| extract_port(e));

        // 有地址和端口时注册 HTTP 健康检查，Consul 据此判定实例存活；
        // 健康检查协议随服务端点派生（https 端点 → https 探活）
        let check = port.map(|p| {
            // IPv6 地址需加方括号才能组成合法 URL 主机
            let host = if address.contains(':') {
                format!("[{address}]")
            } else {
                address.clone()
            };
            let scheme = if endpoint.is_some_and(|e| e.starts_with("https://")) {
                "https"
            } else {
                "http"
            };
            ConsulHealthCheck {
                http: format!("{scheme}://{host}:{p}/health"),
                interval: "10s".into(),
                timeout: "3s".into(),
            }
        });

        let req = ConsulRegisterRequest {
            id: id.clone(),
            name: service.name.clone(),
            address,
            port,
            check,
        };

        let url = format!("{}/v1/agent/service/register", self.base_url);
        let mut builder = self.client.put(&url).json(&req);
        for (k, v) in self.headers() {
            builder = builder.header(k, v);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| RegistryError::Other(format!("consul register: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RegistryError::Other(format!(
                "consul register failed: {body}"
            )));
        }

        Ok(Registration::new(id, service, Arc::new(self.clone())))
    }

    async fn deregister(&self, id: &str) -> Result<(), RegistryError> {
        let url = format!("{}/v1/agent/service/deregister/{id}", self.base_url);
        let mut builder = self.client.put(&url);
        for (k, v) in self.headers() {
            builder = builder.header(k, v);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| RegistryError::Other(format!("consul deregister: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RegistryError::Other(format!(
                "consul deregister failed: {body}"
            )));
        }

        Ok(())
    }

    async fn discover(&self, name: &str) -> Result<Vec<ServiceInfo>, RegistryError> {
        let url = format!(
            "{}/v1/health/service/{}?dc={}&passing=true",
            self.base_url,
            percent_encode_segment(name),
            percent_encode_segment(&self.datacenter)
        );
        let mut builder = self.client.get(&url);
        for (k, v) in self.headers() {
            builder = builder.header(k, v);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| RegistryError::Other(format!("consul discover: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RegistryError::Other(format!(
                "consul discover failed: {body}"
            )));
        }

        let entries: Vec<ConsulHealthEntry> = resp
            .json()
            .await
            .map_err(|e| RegistryError::Other(format!("consul parse: {e}")))?;

        let services = entries
            .into_iter()
            .map(|e| {
                let addr = e.service.address.as_deref().unwrap_or(&e.node.address);
                // IPv6 字面量需加方括号才能组成合法 URL 主机
                let host = if addr.contains(':') {
                    format!("[{addr}]")
                } else {
                    addr.to_string()
                };
                // 服务带 "https" tag 时用 https 协议；Consul 无原生协议字段，
                // 以显式 tag 为准（缺省 http）。
                let scheme = if e.service.tags.iter().any(|t| t == "https") {
                    "https"
                } else {
                    "http"
                };
                let endpoint = format!("{scheme}://{host}:{}", e.service.port);
                // Consul has no native version field; parse it from a
                // "version=x" service tag when present, else leave it empty.
                let version = e
                    .service
                    .tags
                    .iter()
                    .find_map(|t| t.strip_prefix("version="))
                    .unwrap_or("");
                ServiceInfo::new(&e.service.service, version).with_endpoint(endpoint)
            })
            .collect();

        Ok(services)
    }

    async fn list_services(&self) -> Result<Vec<String>, RegistryError> {
        let url = format!("{}/v1/agent/services", self.base_url);
        let mut builder = self.client.get(&url);
        for (k, v) in self.headers() {
            builder = builder.header(k, v);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| RegistryError::Other(format!("consul list: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RegistryError::Other(format!("consul list failed: {body}")));
        }

        let services: HashMap<String, ConsulServiceEntry> = resp
            .json()
            .await
            .map_err(|e| RegistryError::Other(format!("consul parse: {e}")))?;

        Ok(services.into_values().map(|s| s.service).collect())
    }
}

fn is_loopback_host(host: &str) -> bool {
    // url crate 的 host_str() 对 IPv6 保留方括号（"[::1]"）
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

fn extract_host(endpoint: &str) -> &str {
    let rest = endpoint
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    // IPv6 字面量形如 [::1]:8080：取方括号内部分
    if let Some(rest) = rest.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("127.0.0.1");
    }
    rest.split(':').next().unwrap_or("127.0.0.1")
}

fn extract_port(endpoint: &str) -> Option<u32> {
    endpoint
        .rsplit(':')
        .next()
        // 丢弃路径段（如 :8080/health），仅保留端口数字
        .and_then(|p| p.split('/').next())
        .and_then(|p| p.parse().ok())
}

// ── Consul API types ──

#[derive(Debug, Serialize)]
struct ConsulRegisterRequest {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Address")]
    address: String,
    #[serde(rename = "Port", skip_serializing_if = "Option::is_none")]
    port: Option<u32>,
    #[serde(rename = "Check", skip_serializing_if = "Option::is_none")]
    check: Option<ConsulHealthCheck>,
}

#[derive(Debug, Serialize)]
struct ConsulHealthCheck {
    #[serde(rename = "HTTP")]
    http: String,
    #[serde(rename = "Interval")]
    interval: String,
    #[serde(rename = "Timeout")]
    timeout: String,
}

/// Percent-encode a single URL path segment (RFC 3986): every byte except
/// unreserved characters (`A-Z a-z 0-9 - _ . ~`) becomes `%XX`.
fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct ConsulServiceEntry {
    #[serde(rename = "Service")]
    service: String,
}

#[derive(Debug, Deserialize)]
struct ConsulHealthEntry {
    #[serde(rename = "Node")]
    node: ConsulNode,
    #[serde(rename = "Service")]
    service: ConsulHealthService,
}

#[derive(Debug, Deserialize)]
struct ConsulNode {
    #[serde(rename = "Address")]
    address: String,
}

#[derive(Debug, Deserialize)]
struct ConsulHealthService {
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "Address")]
    address: Option<String>,
    #[serde(rename = "Port")]
    port: u16,
    #[serde(rename = "Tags")]
    tags: Vec<String>,
}

#[cfg(test)]
mod tests;

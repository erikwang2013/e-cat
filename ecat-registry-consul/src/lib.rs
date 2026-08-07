// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_registry::{Registration, Registry, RegistryError, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct ConsulRegistry {
    client: reqwest::Client,
    base_url: String,
    datacenter: String,
    token: Option<String>,
}

impl ConsulRegistry {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            datacenter: "dc1".into(),
            token: None,
        }
    }

    pub fn datacenter(self, dc: impl Into<String>) -> Self {
        Self {
            datacenter: dc.into(),
            ..self
        }
    }

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

        let req = ConsulRegisterRequest {
            id: id.clone(),
            name: service.name.clone(),
            address,
            port,
            check: None,
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
            "{}/v1/health/service/{name}?dc={}&passing=true",
            self.base_url, self.datacenter
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
mod tests {
    use super::*;

    #[test]
    fn consul_registry_constructs() {
        let _reg = ConsulRegistry::new("http://localhost:8500")
            .datacenter("dc2")
            .token("secret-token");
    }

    #[test]
    fn consul_registry_clone() {
        let reg = ConsulRegistry::new("http://localhost:8500");
        let _reg2 = reg.clone();
    }

    #[test]
    fn extract_host_ipv4() {
        assert_eq!(extract_host("http://10.0.0.5:8080"), "10.0.0.5");
        assert_eq!(extract_host("https://example.com:443"), "example.com");
        assert_eq!(extract_host("http://10.0.0.5:8080/health"), "10.0.0.5");
    }

    #[test]
    fn extract_host_ipv6_literal() {
        assert_eq!(extract_host("http://[::1]:8080"), "::1");
        assert_eq!(extract_host("https://[2001:db8::1]:443"), "2001:db8::1");
        assert_eq!(extract_host("http://[::1]"), "::1");
    }

    #[test]
    fn extract_port_ipv6() {
        assert_eq!(extract_port("http://[::1]:8080"), Some(8080));
        assert_eq!(extract_port("http://[::1]:8080/health"), Some(8080));
        assert_eq!(extract_port("http://[::1]"), None);
    }

    /// mock Consul 的 /v1/health/service/<name> 端点，返回给定 JSON 文本。
    async fn spawn_mock_health(body: &'static str) -> String {
        let app = axum::Router::new().route(
            "/v1/health/service/{name}",
            axum::routing::get(move || async move {
                axum::response::Response::new(axum::body::Body::from(body))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn discover_uses_https_tag_and_brackets_ipv6() {
        let body = r#"[
            {"Node":{"Address":"10.0.0.5"},
             "Service":{"Service":"web","Address":"2001:db8::1","Port":8443,"Tags":["version=2.0","https"]}}
        ]"#;
        let base_url = spawn_mock_health(body).await;
        let reg = ConsulRegistry::new(base_url);
        let services = reg.discover("web").await.unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].endpoints, vec!["https://[2001:db8::1]:8443"]);
        assert_eq!(services[0].version, "2.0");
    }

    #[tokio::test]
    async fn discover_defaults_to_http() {
        let body = r#"[
            {"Node":{"Address":"10.0.0.5"},
             "Service":{"Service":"api","Address":"10.0.0.9","Port":9000,"Tags":[]}}
        ]"#;
        let base_url = spawn_mock_health(body).await;
        let reg = ConsulRegistry::new(base_url);
        let services = reg.discover("api").await.unwrap();
        assert_eq!(services[0].endpoints, vec!["http://10.0.0.9:9000"]);
    }
}

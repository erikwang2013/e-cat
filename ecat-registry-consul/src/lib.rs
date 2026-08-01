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
        let address = service
            .endpoints
            .first()
            .map(|e| extract_host(e))
            .unwrap_or("127.0.0.1")
            .to_string();

        let req = ConsulRegisterRequest {
            id: id.clone(),
            name: service.name.clone(),
            address,
            port: 0,
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
                let endpoint = format!("http://{addr}:{}", e.service.port);
                ServiceInfo::new(&e.service.service, "1.0").with_endpoint(endpoint)
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

        let services: HashMap<String, ConsulServiceEntry> = resp
            .json()
            .await
            .map_err(|e| RegistryError::Other(format!("consul parse: {e}")))?;

        Ok(services.into_values().map(|s| s.service).collect())
    }
}

fn extract_host(endpoint: &str) -> &str {
    endpoint
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split(':')
        .next()
        .unwrap_or("127.0.0.1")
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
    #[serde(rename = "Port")]
    port: u32,
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
#[allow(dead_code)]
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
}

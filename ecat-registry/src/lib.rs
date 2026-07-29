mod memory;

pub use memory::MemoryRegistry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub version: String,
    pub endpoints: Vec<String>,
    pub metadata: std::collections::HashMap<String, String>,
}

impl ServiceInfo {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            endpoints: Vec::new(),
            metadata: std::collections::HashMap::new(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoints.push(endpoint.into());
        self
    }
}

pub struct Registration {
    pub id: String,
    pub service: ServiceInfo,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // auto-deregister on drop — handled by the registry implementation
    }
}

#[async_trait]
pub trait Registry: Send + Sync {
    async fn register(&self, service: ServiceInfo) -> Result<Registration, RegistryError>;
    async fn deregister(&self, id: &str) -> Result<(), RegistryError>;
    async fn discover(&self, name: &str) -> Result<Vec<ServiceInfo>, RegistryError>;
    async fn list_services(&self) -> Result<Vec<String>, RegistryError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("service not found: {0}")]
    NotFound(String),
    #[error("registry error: {0}")]
    Other(String),
}

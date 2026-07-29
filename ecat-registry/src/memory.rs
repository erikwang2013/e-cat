use crate::{Registry, RegistryError, Registration, ServiceInfo};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

pub struct MemoryRegistry {
    services: Arc<RwLock<HashMap<String, ServiceInfo>>>,
}

impl Default for MemoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryRegistry {
    pub fn new() -> Self {
        Self {
            services: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Registry for MemoryRegistry {
    async fn register(&self, service: ServiceInfo) -> Result<Registration, RegistryError> {
        let id = Uuid::new_v4().to_string();
        let mut services = self.services.write().map_err(|e| {
            RegistryError::Other(format!("lock poisoned: {}", e))
        })?;
        services.insert(id.clone(), service.clone());
        Ok(Registration { id, service })
    }

    async fn deregister(&self, id: &str) -> Result<(), RegistryError> {
        let mut services = self.services.write().map_err(|e| {
            RegistryError::Other(format!("lock poisoned: {}", e))
        })?;
        services.remove(id).ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        Ok(())
    }

    async fn discover(&self, name: &str) -> Result<Vec<ServiceInfo>, RegistryError> {
        let services = self.services.read().map_err(|e| {
            RegistryError::Other(format!("lock poisoned: {}", e))
        })?;
        let results: Vec<ServiceInfo> = services
            .values()
            .filter(|s| s.name == name)
            .cloned()
            .collect();
        Ok(results)
    }

    async fn list_services(&self) -> Result<Vec<String>, RegistryError> {
        let services = self.services.read().map_err(|e| {
            RegistryError::Other(format!("lock poisoned: {}", e))
        })?;
        let names: Vec<String> = services.values().map(|s| s.name.clone()).collect();
        Ok(names)
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_transport::Server;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct MockServer {
    running: Arc<AtomicBool>,
}

impl MockServer {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Default for MockServer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Server for MockServer {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
}

pub struct ChaosConfig {
    pub latency_ms: Option<u64>,
    pub error_rate: f64,
    pub enabled: bool,
}

impl Default for ChaosConfig {
    fn default() -> Self {
        Self {
            latency_ms: None,
            error_rate: 0.0,
            enabled: false,
        }
    }
}

impl ChaosConfig {
    pub fn with_latency(mut self, ms: u64) -> Self {
        self.latency_ms = Some(ms);
        self.enabled = true;
        self
    }
    pub fn with_errors(mut self, rate: f64) -> Self {
        self.error_rate = rate.clamp(0.0, 1.0);
        self.enabled = true;
        self
    }
    pub fn should_fail(&self) -> bool {
        if !self.enabled || self.error_rate == 0.0 {
            return false;
        }
        rand_fraction() < self.error_rate
    }
    pub fn latency(&self) -> Option<std::time::Duration> {
        self.latency_ms.map(std::time::Duration::from_millis)
    }
}

fn rand_fraction() -> f64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::Instant::now().hash(&mut h);
    (h.finish() % 10000) as f64 / 10000.0
}

pub struct TestFixture {
    pub app_name: String,
    pub mock_server: MockServer,
    pub chaos: ChaosConfig,
}

impl TestFixture {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            app_name: name.into(),
            mock_server: MockServer::new(),
            chaos: ChaosConfig::default(),
        }
    }
    pub fn with_chaos(mut self, chaos: ChaosConfig) -> Self {
        self.chaos = chaos;
        self
    }
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.mock_server.start().await
    }
    pub async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.mock_server.stop().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_server_constructs() {
        let srv = MockServer::new();
        assert!(!srv.is_running());
    }

    #[tokio::test]
    async fn mock_server_start_stop() {
        let srv = MockServer::new();
        srv.start().await.unwrap();
        assert!(srv.is_running());
        srv.stop().await.unwrap();
        assert!(!srv.is_running());
    }

    #[test]
    fn chaos_default_disabled() {
        let c = ChaosConfig::default();
        assert!(!c.enabled);
    }

    #[test]
    fn chaos_with_errors() {
        let c = ChaosConfig::default().with_errors(1.0);
        assert!(c.enabled);
    }

    #[test]
    fn test_fixture_defaults() {
        let f = TestFixture::new("test-app");
        assert_eq!(f.app_name, "test-app");
    }
}

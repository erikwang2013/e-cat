// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
mod context;
mod request;
mod response;

use std::path::PathBuf;

pub use context::Context;
pub use request::Request;
pub use response::Response;

use async_trait::async_trait;

#[async_trait]
pub trait Server: Send + Sync {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

// ── mTLS Configuration ──

#[derive(Clone)]
pub struct TlsConfig {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub ca_cert_path: Option<PathBuf>,
    pub require_client_auth: bool,
}

impl TlsConfig {
    pub fn new(cert_path: impl Into<PathBuf>, key_path: impl Into<PathBuf>) -> Self {
        Self {
            cert_path: cert_path.into(),
            key_path: key_path.into(),
            ca_cert_path: None,
            require_client_auth: false,
        }
    }

    pub fn with_client_auth(mut self, ca_cert_path: impl Into<PathBuf>) -> Self {
        self.ca_cert_path = Some(ca_cert_path.into());
        self.require_client_auth = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestServer;

    #[async_trait]
    impl Server for TestServer {
        async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_server_trait_start() {
        let server = TestServer;
        assert!(server.start().await.is_ok());
    }

    #[tokio::test]
    async fn test_server_trait_stop() {
        let server = TestServer;
        assert!(server.stop().await.is_ok());
    }
}

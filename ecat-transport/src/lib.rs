// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
mod context;
mod request;
mod response;

pub use context::Context;
pub use request::Request;
pub use response::Response;

use async_trait::async_trait;

#[async_trait]
pub trait Server: Send + Sync {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
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

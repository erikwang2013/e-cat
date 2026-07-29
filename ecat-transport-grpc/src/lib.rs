// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use ecat_transport::Server as TransportServer;
use tonic::service::Routes;
use tonic::transport::Server as TonicServer;

pub struct GrpcServer {
    addr: String,
}

impl GrpcServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self { addr: addr.into() }
    }
}

#[async_trait::async_trait]
impl TransportServer for GrpcServer {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr = self.addr.parse()?;
        let mut server = TonicServer::builder();
        let router = server.add_routes(Routes::default());
        router.serve(addr).await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

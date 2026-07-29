// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use ecat_transport::Server as TransportServer;
use tonic::service::Routes;

pub struct GrpcServer {
    addr: String,
    routes: Option<Routes>,
}

impl GrpcServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self { addr: addr.into(), routes: None }
    }

    pub fn routes(mut self, routes: Routes) -> Self {
        self.routes = Some(routes);
        self
    }
}

#[async_trait::async_trait]
impl TransportServer for GrpcServer {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr = self.addr.parse()?;
        let routes = self.routes.clone().unwrap_or_default();
        tonic::transport::Server::builder()
            .add_routes(routes)
            .serve(addr)
            .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

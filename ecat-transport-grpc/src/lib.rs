// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use ecat_transport::Server as TransportServer;
use std::sync::Mutex;
use tokio::sync::watch;
use tonic::service::Routes;

pub struct GrpcServer {
    addr: String,
    routes: Option<Routes>,
    shutdown_tx: Mutex<Option<watch::Sender<()>>>,
}

impl GrpcServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            routes: None,
            shutdown_tx: Mutex::new(None),
        }
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
        let (tx, mut rx) = watch::channel(());
        *self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let shutdown_signal = async move {
            let _ = rx.changed().await;
        };
        tonic::transport::Server::builder()
            .add_routes(routes)
            .serve_with_shutdown(addr, shutdown_signal)
            .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(tx) = self
            .shutdown_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = tx.send(());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_addr() {
        let srv = GrpcServer::new("0.0.0.0:50051");
        assert_eq!(srv.addr, "0.0.0.0:50051");
    }

    #[test]
    fn routes_sets_routes() {
        let routes = tonic::service::Routes::default();
        let srv = GrpcServer::new("0.0.0.0:50051").routes(routes);
        assert!(srv.routes.is_some());
    }

    #[test]
    fn new_without_routes_has_none() {
        let srv = GrpcServer::new("0.0.0.0:50051");
        assert!(srv.routes.is_none());
    }
}

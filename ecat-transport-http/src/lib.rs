// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use axum::Router;
use ecat_transport::{Server as TransportServer, TlsConfig};
use std::sync::Mutex;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub struct HttpServer {
    addr: String,
    router: Option<Router>,
    shutdown_tx: Mutex<Option<watch::Sender<()>>>,
    tls_config: Option<TlsConfig>,
}

impl HttpServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            router: None,
            shutdown_tx: Mutex::new(None),
            tls_config: None,
        }
    }

    pub fn router(mut self, router: Router) -> Self {
        self.router = Some(router);
        self
    }

    pub fn tls(mut self, config: TlsConfig) -> Self {
        self.tls_config = Some(config);
        self
    }
}

#[async_trait::async_trait]
impl TransportServer for HttpServer {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let router = self.router.clone().unwrap_or_default();
        let listener = TcpListener::bind(&self.addr).await?;
        let (tx, mut rx) = watch::channel(());
        *self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let shutdown_signal = async move {
            let _ = rx.changed().await;
        };
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal)
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
    use axum::{response::IntoResponse, routing::get};

    async fn health() -> impl IntoResponse {
        "ok"
    }

    #[test]
    fn new_sets_addr() {
        let srv = HttpServer::new("0.0.0.0:9000");
        assert_eq!(srv.addr, "0.0.0.0:9000");
    }

    #[test]
    fn router_sets_router() {
        let router = Router::new().route("/health", get(health));
        let srv = HttpServer::new("0.0.0.0:9000").router(router);
        assert!(srv.router.is_some());
    }

    #[test]
    fn new_without_router_has_none() {
        let srv = HttpServer::new("0.0.0.0:9000");
        assert!(srv.router.is_none());
    }
}

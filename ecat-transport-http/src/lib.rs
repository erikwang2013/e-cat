// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use axum::Router;
use ecat_transport::Server as TransportServer;
use std::sync::Mutex;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub struct HttpServer {
    addr: String,
    router: Option<Router>,
    shutdown_tx: Mutex<Option<watch::Sender<()>>>,
}

impl HttpServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            router: None,
            shutdown_tx: Mutex::new(None),
        }
    }

    pub fn router(mut self, router: Router) -> Self {
        self.router = Some(router);
        self
    }
}

#[async_trait::async_trait]
impl TransportServer for HttpServer {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let router = self.router.clone().unwrap_or_default();
        let listener = TcpListener::bind(&self.addr).await?;
        let (tx, mut rx) = watch::channel(());
        *self.shutdown_tx.lock().unwrap() = Some(tx);
        let shutdown_signal = async move {
            let _ = rx.changed().await;
        };
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal)
            .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(tx) = self.shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

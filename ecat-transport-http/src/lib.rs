use axum::Router;
use ecat_transport::Server as TransportServer;
use tokio::net::TcpListener;

pub struct HttpServer {
    addr: String,
    router: Option<Router>,
}

impl HttpServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            router: None,
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
        let router = self.router.clone().unwrap_or_else(Router::new);
        let listener = TcpListener::bind(&self.addr).await?;
        axum::serve(listener, router).await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

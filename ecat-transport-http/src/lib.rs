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
            // 空 host（如 ":8000"）会解析到 IPv6 通配 [::]，在无 IPv6 环境绑定失败；
            // 规范化为 IPv4 通配 "0.0.0.0"
            addr: normalize_addr(addr.into()),
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

/// 将空 host 的地址（":8000"）规范化为 IPv4 通配（"0.0.0.0:8000"），
/// 避免解析到 IPv6 [::] 而在无 IPv6 环境绑定失败。
fn normalize_addr(addr: String) -> String {
    if addr.starts_with(':') {
        format!("0.0.0.0{addr}")
    } else {
        addr
    }
}

impl HttpServer {
    /// 用户 router 与内置 /metrics 端点合并。
    /// /metrics 为框架保留路径：用户 router 若也定义该路径，merge 会 panic。
    fn merged_router(&self) -> Router {
        self.router
            .clone()
            .unwrap_or_default()
            .merge(ecat_metrics::metrics_router())
    }
}

#[async_trait::async_trait]
impl TransportServer for HttpServer {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let router = self.merged_router();
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
    fn new_normalizes_bare_port_to_ipv4_wildcard() {
        let srv = HttpServer::new(":9000");
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

    #[tokio::test]
    async fn auto_mounts_metrics_endpoint() {
        use tower::ServiceExt;
        let srv = HttpServer::new("127.0.0.1:0");
        let resp = srv
            .merged_router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }
}

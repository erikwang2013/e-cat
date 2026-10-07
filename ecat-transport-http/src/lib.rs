// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use axum::Router;
use ecat_transport::{Server as TransportServer, TlsConfig, normalize_addr};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::watch;

mod tls_listener;

use tls_listener::build_server_config;

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

impl HttpServer {
    /// 用户 router 与内置 /metrics 端点合并。
    /// /metrics 为框架保留路径：用户 router 若也定义该路径，merge 会 panic，
    /// 此时捕获 panic、记录 warn 并降级为用户 router（不挂 /metrics）。
    fn merged_router(&self) -> Router {
        let user = self.router.clone().unwrap_or_default();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            user.clone().merge(ecat_metrics::metrics_router())
        })) {
            Ok(merged) => merged,
            Err(_) => {
                tracing::warn!(
                    "user router defines /metrics; serving user route, framework metrics disabled"
                );
                user
            }
        }
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
        if let Some(tls) = &self.tls_config {
            let server_config = build_server_config(tls)?;
            let tls_listener = tls_listener::TlsListener::new(
                listener,
                tokio_rustls::TlsAcceptor::from(Arc::new(server_config)),
            );
            axum::serve(tls_listener, router)
                .with_graceful_shutdown(shutdown_signal)
                .await?;
        } else {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal)
                .await?;
        }
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
mod tests;

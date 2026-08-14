// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::routing::get;
use ecat_transport::{normalize_addr, Server as TransportServer};
use std::sync::Arc;
use tokio::net::TcpListener;

pub type WsHandler = Arc<
    dyn Fn(WebSocket) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

pub struct WsServer {
    addr: String,
    path: String,
    handler: Option<WsHandler>,
    shutdown_tx: std::sync::Mutex<Option<tokio::sync::watch::Sender<()>>>,
    serve_task: std::sync::Mutex<Option<tokio::task::JoinHandle<std::io::Result<()>>>>,
}

impl WsServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            // 空 host（如 ":3000"）会解析到 IPv6 通配 [::]，在无 IPv6 环境
            // 绑定失败；规范化为 IPv4 通配 "0.0.0.0"
            addr: normalize_addr(addr.into()),
            path: "/ws".into(),
            handler: None,
            shutdown_tx: std::sync::Mutex::new(None),
            serve_task: std::sync::Mutex::new(None),
        }
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    pub fn handler(mut self, handler: WsHandler) -> Self {
        self.handler = Some(handler);
        self
    }
}

#[async_trait]
impl TransportServer for WsServer {
    async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let handler = self.handler.clone().ok_or("ws handler not set")?;
        let path = self.path.clone();

        let app = axum::Router::new().route(
            &path,
            get(move |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(move |socket| async move { handler(socket).await })
            }),
        );

        let listener = TcpListener::bind(&self.addr).await?;
        let (tx, mut rx) = tokio::sync::watch::channel(());
        *self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let shutdown_signal = async move {
            let _ = rx.changed().await;
        };
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal)
                .await
        });
        *self.serve_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
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
        let handle = self
            .serve_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(handle) = handle {
            handle
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)??;
        }
        Ok(())
    }
}

pub fn echo_handler() -> WsHandler {
    Arc::new(|mut ws: WebSocket| {
        Box::pin(async move {
            while let Some(Ok(msg)) = ws.recv().await {
                if let Message::Text(text) = msg {
                    let _ = ws.send(Message::Text(text)).await;
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_server_constructs() {
        let srv = WsServer::new("0.0.0.0:3000")
            .path("/chat")
            .handler(echo_handler());
        assert_eq!(srv.path, "/chat");
    }

    /// N3：空 host 地址（":3000"）必须规范化为 IPv4 通配，与 HttpServer 一致。
    #[test]
    fn new_normalizes_empty_host_addr() {
        let srv = WsServer::new(":3000");
        assert_eq!(srv.addr, "0.0.0.0:3000");
    }
}

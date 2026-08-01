// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::routing::get;
use ecat_transport::Server as TransportServer;
use std::sync::Arc;
use tokio::net::TcpListener;

pub type WsHandler = Arc<
    dyn Fn(WebSocket) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

pub struct WsServer {
    addr: String,
    path: String,
    handler: Option<WsHandler>,
}

impl WsServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            path: "/ws".into(),
            handler: None,
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
        axum::serve(listener, app).await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
}

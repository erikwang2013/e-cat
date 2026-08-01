// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::broadcast;

#[async_trait]
pub trait MessageQueue: Send + Sync {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), MqError>;
    async fn subscribe(&self, topic: &str) -> Result<Box<dyn MessageStream>, MqError>;
}

pub trait MessageStream: Send + Unpin {
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Vec<u8>, MqError>>>;
}

#[derive(Debug, thiserror::Error)]
pub enum MqError {
    #[error("mq error: {0}")]
    Other(String),
}

pub struct InMemoryMq {
    senders: std::sync::RwLock<HashMap<String, Vec<broadcast::Sender<Vec<u8>>>>>,
}

impl InMemoryMq {
    pub fn new() -> Self {
        Self {
            senders: std::sync::RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryMq {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MessageQueue for InMemoryMq {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), MqError> {
        let map = self.senders.read().map_err(|e| MqError::Other(e.to_string()))?;
        if let Some(txs) = map.get(topic) {
            let data = payload.to_vec();
            for tx in txs {
                let _ = tx.send(data.clone());
            }
        }
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<Box<dyn MessageStream>, MqError> {
        let (tx, rx) = broadcast::channel(256);
        let mut map = self.senders.write().map_err(|e| MqError::Other(e.to_string()))?;
        map.entry(topic.to_string()).or_default().push(tx);
        Ok(Box::new(InMemoryStream { rx }))
    }
}

struct InMemoryStream {
    rx: broadcast::Receiver<Vec<u8>>,
}

impl MessageStream for InMemoryStream {
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Vec<u8>, MqError>>> {
        match self.rx.try_recv() {
            Ok(data) => Poll::Ready(Some(Ok(data))),
            Err(broadcast::error::TryRecvError::Empty) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(broadcast::error::TryRecvError::Closed) => Poll::Ready(None),
            Err(broadcast::error::TryRecvError::Lagged(_)) => Poll::Pending,
        }
    }
}

impl Unpin for InMemoryStream {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inmemory_pub_sub() {
        let mq = InMemoryMq::new();
        mq.publish("test", b"hello").await.unwrap();
        let _stream = mq.subscribe("test").await.unwrap();
    }

    #[test]
    fn mq_error_display() {
        let e = MqError::Other("boom".into());
        assert!(e.to_string().contains("boom"));
    }
}

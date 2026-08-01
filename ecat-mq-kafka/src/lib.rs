// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_mq::{MessageQueue, MessageStream, MqError};
use std::collections::HashMap;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

pub struct KafkaMq {
    topics: tokio::sync::Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>,
}

impl KafkaMq {
    pub fn new() -> Self {
        Self {
            topics: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl Default for KafkaMq {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MessageQueue for KafkaMq {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), MqError> {
        let map = self.topics.lock().await;
        if let Some(tx) = map.get(topic) {
            tx.send(payload.to_vec())
                .await
                .map_err(|e| MqError::Other(format!("kafka send: {e}")))?;
        }
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<Box<dyn MessageStream>, MqError> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1024);
        self.topics.lock().await.insert(topic.to_string(), tx);
        Ok(Box::new(KafkaStream { rx }))
    }
}

struct KafkaStream {
    rx: mpsc::Receiver<Vec<u8>>,
}

impl MessageStream for KafkaStream {
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Vec<u8>, MqError>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(data)) => Poll::Ready(Some(Ok(data))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Unpin for KafkaStream {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kafka_mq_constructs() {
        let _mq = KafkaMq::new();
    }
}

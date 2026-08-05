// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_mq::{MessageQueue, MessageStream, MqError};
use rdkafka::Message;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{BaseConsumer, Consumer};
use rdkafka::producer::{FutureProducer, FutureRecord};
use serde::Deserialize;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Deserialize)]
pub struct KafkaConfig {
    pub brokers: String,
    #[serde(default)]
    pub group_id: Option<String>,
}

pub struct KafkaMq {
    producer: FutureProducer,
    brokers: String,
    group_id: Option<String>,
}

impl KafkaMq {
    pub async fn connect(brokers: &str) -> Result<Self, MqError> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("message.timeout.ms", "5000")
            .create()
            .map_err(|e| MqError::Other(format!("kafka producer: {e}")))?;
        Ok(Self {
            producer,
            brokers: brokers.to_string(),
            group_id: None,
        })
    }

    pub fn from_config(cfg: KafkaConfig) -> Result<Self, MqError> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", &cfg.brokers)
            .set("message.timeout.ms", "5000")
            .create()
            .map_err(|e| MqError::Other(format!("kafka producer: {e}")))?;
        Ok(Self {
            producer,
            brokers: cfg.brokers,
            group_id: cfg.group_id,
        })
    }
}

#[async_trait]
impl MessageQueue for KafkaMq {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), MqError> {
        let record: FutureRecord<'_, str, [u8]> = FutureRecord::to(topic).payload(payload);
        self.producer
            .send(record, Duration::from_secs(5))
            .await
            .map_err(|(e, _)| MqError::Other(format!("kafka publish: {e}")))?;
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<Box<dyn MessageStream>, MqError> {
        let mut config = ClientConfig::new();
        config
            .set("bootstrap.servers", &self.brokers)
            .set("enable.auto.commit", "false")
            .set("auto.offset.reset", "latest");
        if let Some(group) = &self.group_id {
            config.set("group.id", group);
        }
        let consumer: BaseConsumer = config
            .create()
            .map_err(|e| MqError::Other(format!("kafka consumer: {e}")))?;
        consumer
            .subscribe(&[topic])
            .map_err(|e| MqError::Other(format!("kafka subscribe: {e}")))?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>(1024);
        tokio::spawn(async move {
            loop {
                if let Some(Ok(msg)) = consumer.poll(Duration::from_millis(100))
                    && let Some(payload) = msg.payload()
                    && tx.send(payload.to_vec()).await.is_err()
                {
                    break;
                }
                // Yield the worker thread between polls.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
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
    fn config_deserializes() {
        let cfg: KafkaConfig = serde_json::from_value(serde_json::json!({
            "brokers": "localhost:9092",
            "group_id": "my-group",
        }))
        .unwrap();
        assert_eq!(cfg.group_id.as_deref(), Some("my-group"));
    }

    #[tokio::test]
    async fn producer_constructs() {
        let _mq = KafkaMq::connect("localhost:9092").await.unwrap();
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_mq::{MessageQueue, MessageStream, MqError};
use rdkafka::Message;
use rdkafka::config::ClientConfig;
use futures_util::StreamExt;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::producer::{FutureProducer, FutureRecord};
use serde::Deserialize;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

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

    pub async fn from_config(cfg: KafkaConfig) -> Result<Self, MqError> {
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
        config.set(
            "group.id",
            consumer_group_id(self.group_id.as_deref(), topic),
        );
        let consumer: StreamConsumer = config
            .create()
            .map_err(|e| MqError::Other(format!("kafka consumer: {e}")))?;
        consumer
            .subscribe(&[topic])
            .map_err(|e| MqError::Other(format!("kafka subscribe: {e}")))?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>(1024);
        tokio::spawn(async move {
            // StreamConsumer 由 tokio 驱动：消息到达立即唤醒，空闲时挂起，
            // 无固定 poll/sleep 延迟，也不阻塞 tokio worker 线程。
            let mut stream = consumer.stream();
            while let Some(msg) = stream.next().await {
                let Ok(msg) = msg else { continue };
                if let Some(payload) = msg.payload()
                    && tx.send(payload.to_vec()).await.is_err()
                {
                    break;
                }
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

fn consumer_group_id(group_id: Option<&str>, topic: &str) -> String {
    match group_id {
        Some(g) => format!("{g}-{topic}"),
        None => format!("ecat-mq-{}", Uuid::new_v4()),
    }
}

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

    #[test]
    fn group_id_without_configured_group_is_random_and_unique() {
        let a = consumer_group_id(None, "user.created");
        let b = consumer_group_id(None, "user.created");
        assert_ne!(a, b);
        assert!(a.starts_with("ecat-mq-"), "got: {a}");
        // 不同 topic 同样各得独立消费组
        assert_ne!(consumer_group_id(None, "order.paid"), a);
    }

    #[test]
    fn group_id_derives_configured_group_per_topic() {
        assert_eq!(
            consumer_group_id(Some("my-group"), "user.created"),
            "my-group-user.created"
        );
        // 同一 (group, topic) 幂等 → 多实例/多订阅共享消费组负载均衡
        assert_eq!(
            consumer_group_id(Some("my-group"), "user.created"),
            consumer_group_id(Some("my-group"), "user.created")
        );
        // 不同 topic 必须隔离，避免同组 roundrobin 把消息分给错误订阅者
        assert_ne!(
            consumer_group_id(Some("my-group"), "user.created"),
            consumer_group_id(Some("my-group"), "order.paid")
        );
    }

    #[tokio::test]
    async fn producer_constructs() {
        let _mq = KafkaMq::connect("localhost:9092").await.unwrap();
    }

    #[tokio::test]
    async fn stream_consumer_constructs_with_derived_group() {
        // 锁定订阅路径构造：无配置 group_id 时也能创建 StreamConsumer（
        // rdkafka create() 只校验配置不连 broker），避免回归到 INVALID_ARG。
        let mut config = ClientConfig::new();
        config
            .set("bootstrap.servers", "localhost:9092")
            .set("enable.auto.commit", "false")
            .set("auto.offset.reset", "latest")
            .set("group.id", consumer_group_id(None, "test.topic"));
        let consumer: StreamConsumer = config.create().unwrap();
        consumer.subscribe(&["test.topic"]).unwrap();
    }
}

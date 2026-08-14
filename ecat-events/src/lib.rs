// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use ecat_mq::MessageQueue;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

type Handler = Arc<
    dyn Fn(Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

pub struct EventBus {
    mq: Option<Arc<dyn MessageQueue>>,
    local_handlers: Arc<RwLock<HashMap<String, Vec<Handler>>>>,
    /// 远程模式消费占位集合：同一事件类型只启动一个消费任务。
    /// 消费任务退出（正常结束或 panic）后由监护任务移除占位，
    /// 使再次 subscribe 能重启消费。
    consumers: Arc<Mutex<HashMap<String, ()>>>,
}

impl EventBus {
    pub fn local() -> Self {
        Self {
            mq: None,
            local_handlers: Arc::new(RwLock::new(HashMap::new())),
            consumers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn remote(mq: Arc<dyn MessageQueue>) -> Self {
        Self {
            mq: Some(mq),
            local_handlers: Arc::new(RwLock::new(HashMap::new())),
            consumers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn subscribe<E, F, Fut>(&self, handler: F)
    where
        E: DeserializeOwned + Send + 'static,
        F: Fn(E) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let event_name = std::any::type_name::<E>().to_string();
        let handler: Handler = Arc::new(move |data: Vec<u8>| {
            let event: E = match serde_json::from_slice(&data) {
                Ok(e) => e,
                Err(err) => {
                    tracing::error!(%err, "failed to deserialize event");
                    return Box::pin(async {});
                }
            };
            let fut = handler(event);
            Box::pin(fut)
        });

        self.local_handlers
            .write()
            .await
            .entry(event_name.clone())
            .or_default()
            .push(handler);

        // 远程模式下为每个事件类型启动一个消费任务：从 mq 收消息并分发到
        // 已注册的本地 handler。同一类型只启动一次。mq 订阅在 subscribe 返回
        // 前完成，保证订阅之后的发布都能被消费。
        if let Some(mq) = &self.mq {
            // 先占位再 await：防止并发 subscribe 为同一类型启动两个消费任务。
            // 作用域块保证锁在 await 前释放。
            {
                let mut consumers = self.consumers.lock().unwrap_or_else(|e| e.into_inner());
                if consumers.contains_key(&event_name) {
                    return;
                }
                consumers.insert(event_name.clone(), ());
            }

            let mut stream = match mq.subscribe(&event_name).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(%e, "mq subscribe failed");
                    // 回滚占位，允许后续 subscribe 重试。
                    self.consumers
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&event_name);
                    return;
                }
            };
            let handlers = self.local_handlers.clone();
            let topic = event_name.clone();
            let handle = tokio::spawn(async move {
                loop {
                    match std::future::poll_fn(|cx| stream.poll_recv(cx)).await {
                        Some(Ok(payload)) => {
                            let hs = handlers.read().await;
                            if let Some(list) = hs.get(&topic) {
                                for h in list {
                                    let fut = h(payload.clone());
                                    tokio::spawn(fut);
                                }
                            }
                        }
                        Some(Err(e)) => tracing::warn!(%e, "mq receive failed"),
                        None => break,
                    }
                }
            });
            // 监护任务：消费任务退出（正常结束或 panic，JoinHandle 均会返回）
            // 后移除占位，使同类型事件再次 subscribe 能重启消费，
            // 避免事件因残留占位永久静默丢失（N1）。
            let consumers = self.consumers.clone();
            let topic = event_name.clone();
            tokio::spawn(async move {
                let _ = handle.await;
                if consumers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&topic)
                    .is_some()
                {
                    tracing::warn!(
                        event = %topic,
                        "event consumer exited; subscribe again to restart"
                    );
                }
            });
        }
    }

    pub async fn publish<E: Serialize + Send + Sync>(
        &self,
        event: &E,
    ) -> Result<(), EventBusError> {
        let event_name = std::any::type_name::<E>().to_string();
        let payload =
            serde_json::to_vec(event).map_err(|e| EventBusError(format!("serialize: {e}")))?;

        if let Some(ref mq) = self.mq {
            // 远程模式：只发布到 mq，本地 handler 由消费任务回环分发，
            // 避免本地直接分发 + 回环消费导致的重复投递。
            mq.publish(&event_name, &payload)
                .await
                .map_err(|e| EventBusError(format!("mq publish: {e}")))?;
            return Ok(());
        }

        let handlers = self.local_handlers.read().await;
        if let Some(hs) = handlers.get(&event_name) {
            for h in hs {
                let fut = h(payload.clone());
                tokio::spawn(fut);
            }
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("event bus error: {0}")]
pub struct EventBusError(String);

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_mq::{MessageStream, MqError};
    use serde::Deserialize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestEvent {
        id: u32,
    }

    /// 订阅计数型假 MQ：subscribe 立即返回"流结束"（None），
    /// 用于让消费任务正常退出。
    struct EndingMq {
        subscribe_count: Arc<AtomicUsize>,
    }

    struct EndingStream;

    impl MessageStream for EndingStream {
        fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Option<Result<Vec<u8>, MqError>>> {
            Poll::Ready(None)
        }
    }

    #[async_trait::async_trait]
    impl MessageQueue for EndingMq {
        async fn publish(&self, _topic: &str, _payload: &[u8]) -> Result<(), MqError> {
            Ok(())
        }

        async fn subscribe(&self, _topic: &str) -> Result<Box<dyn MessageStream>, MqError> {
            self.subscribe_count.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(EndingStream))
        }
    }

    /// 消费任务 panic 型假 MQ：poll_recv 直接 panic，
    /// 用于让消费任务以 panic 方式退出。
    struct PanicMq {
        subscribe_count: Arc<AtomicUsize>,
    }

    struct PanicStream;

    impl MessageStream for PanicStream {
        fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Option<Result<Vec<u8>, MqError>>> {
            panic!("simulated consumer panic")
        }
    }

    #[async_trait::async_trait]
    impl MessageQueue for PanicMq {
        async fn publish(&self, _topic: &str, _payload: &[u8]) -> Result<(), MqError> {
            Ok(())
        }

        async fn subscribe(&self, _topic: &str) -> Result<Box<dyn MessageStream>, MqError> {
            self.subscribe_count.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(PanicStream))
        }
    }

    /// 等待消费任务退出并清理占位标记（reaper 是异步任务，需轮询）。
    async fn wait_for_consumer_cleanup(bus: &EventBus) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !bus
                .consumers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("consumer marker never cleaned up");
    }

    /// N1 回归：消费任务因 mq 流结束（None）退出后，占位标记必须被移除，
    /// 同类型事件再次 subscribe 能重启消费，事件不会永久静默丢失。
    #[tokio::test]
    async fn consumer_restarts_after_stream_ends() {
        let subscribe_count = Arc::new(AtomicUsize::new(0));
        let mq: Arc<dyn MessageQueue> = Arc::new(EndingMq {
            subscribe_count: subscribe_count.clone(),
        });
        let bus = EventBus::remote(mq);

        bus.subscribe::<TestEvent, _, _>(|_e: TestEvent| async {}).await;
        wait_for_consumer_cleanup(&bus).await;

        bus.subscribe::<TestEvent, _, _>(|_e: TestEvent| async {}).await;
        assert_eq!(
            subscribe_count.load(Ordering::SeqCst),
            2,
            "consumer was not restarted after stream ended"
        );
    }

    /// N1 回归：消费任务以 panic 方式退出后，占位标记同样必须被移除，
    /// 再次 subscribe 能重启消费。
    #[tokio::test]
    async fn consumer_restarts_after_task_panic() {
        let subscribe_count = Arc::new(AtomicUsize::new(0));
        let mq: Arc<dyn MessageQueue> = Arc::new(PanicMq {
            subscribe_count: subscribe_count.clone(),
        });
        let bus = EventBus::remote(mq);

        bus.subscribe::<TestEvent, _, _>(|_e: TestEvent| async {}).await;
        wait_for_consumer_cleanup(&bus).await;

        bus.subscribe::<TestEvent, _, _>(|_e: TestEvent| async {}).await;
        assert_eq!(
            subscribe_count.load(Ordering::SeqCst),
            2,
            "consumer was not restarted after panic"
        );
    }

    #[tokio::test]
    async fn local_pub_sub() {
        let bus = EventBus::local();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();

        bus.subscribe::<TestEvent, _, _>(move |_e: TestEvent| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;

        bus.publish(&TestEvent { id: 42 }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn multiple_handlers() {
        let bus = EventBus::local();
        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = counter.clone();
        let c2 = counter.clone();

        bus.subscribe::<TestEvent, _, _>(move |_e: TestEvent| {
            let c = c1.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;

        bus.subscribe::<TestEvent, _, _>(move |_e: TestEvent| {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;

        bus.publish(&TestEvent { id: 1 }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn remote_publish_delivers_via_mq_consumer() {
        let mq: Arc<dyn MessageQueue> = Arc::new(ecat_mq::InMemoryMq::new());
        let bus = EventBus::remote(mq);
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();

        bus.subscribe::<TestEvent, _, _>(move |_e: TestEvent| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;

        bus.publish(&TestEvent { id: 7 }).await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while counter.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("remote event never delivered");
        // 本地发布只经 mq 回环分发一次，不得重复投递
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn remote_events_from_other_bus_are_delivered() {
        let mq: Arc<dyn MessageQueue> = Arc::new(ecat_mq::InMemoryMq::new());
        let bus = EventBus::remote(mq.clone());
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();

        bus.subscribe::<TestEvent, _, _>(move |_e: TestEvent| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;

        let other = EventBus::remote(mq);
        other.publish(&TestEvent { id: 3 }).await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while counter.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("remote event from other bus never delivered");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}

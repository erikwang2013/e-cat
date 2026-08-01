// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use ecat_mq::MessageQueue;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

type Handler = Arc<
    dyn Fn(Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

pub struct EventBus {
    mq: Option<Arc<dyn MessageQueue>>,
    local_handlers: Arc<RwLock<HashMap<String, Vec<Handler>>>>,
}

impl EventBus {
    pub fn local() -> Self {
        Self {
            mq: None,
            local_handlers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn remote(mq: Arc<dyn MessageQueue>) -> Self {
        Self {
            mq: Some(mq),
            local_handlers: Arc::new(RwLock::new(HashMap::new())),
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
            .entry(event_name)
            .or_default()
            .push(handler);
    }

    pub async fn publish<E: Serialize + Send + Sync>(
        &self,
        event: &E,
    ) -> Result<(), EventBusError> {
        let event_name = std::any::type_name::<E>().to_string();
        let payload =
            serde_json::to_vec(event).map_err(|e| EventBusError(format!("serialize: {e}")))?;

        let handlers = self.local_handlers.read().await;
        if let Some(hs) = handlers.get(&event_name) {
            for h in hs {
                let fut = h(payload.clone());
                tokio::spawn(fut);
            }
        }

        if let Some(ref mq) = self.mq {
            mq.publish(&event_name, &payload)
                .await
                .map_err(|e| EventBusError(format!("mq publish: {e}")))?;
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
    use serde::Deserialize;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestEvent {
        id: u32,
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
}

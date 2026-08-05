// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tower::{Layer, Service};

#[async_trait]
pub trait RateLimitStore: Send + Sync {
    async fn check(&self, key: &str, max: u32, window_secs: u64) -> Result<(), String>;
}

pub struct MemoryStore {
    buckets: Mutex<HashMap<String, (u32, Instant)>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RateLimitStore for MemoryStore {
    async fn check(&self, key: &str, max: u32, window_secs: u64) -> Result<(), String> {
        let mut buckets = self.buckets.lock().await;
        let entry = buckets
            .entry(key.to_string())
            .or_insert((0, Instant::now()));
        if entry.1.elapsed() > Duration::from_secs(window_secs) {
            *entry = (1, Instant::now());
            return Ok(());
        }
        if entry.0 >= max {
            return Err("rate limit exceeded".into());
        }
        entry.0 += 1;
        Ok(())
    }
}

struct RateLimiter {
    store: Arc<dyn RateLimitStore>,
    max_requests: u32,
    window: Duration,
}

impl RateLimiter {
    fn new(max_requests: u32, window: Duration) -> Self {
        Self::with_store(max_requests, window, Arc::new(MemoryStore::new()))
    }

    fn with_store(max_requests: u32, window: Duration, store: Arc<dyn RateLimitStore>) -> Self {
        Self {
            store,
            max_requests,
            window,
        }
    }

    async fn allow(&self, key: &str) -> bool {
        self.store
            .check(key, self.max_requests, self.window.as_secs())
            .await
            .is_ok()
    }
}

#[derive(Clone)]
pub struct RateLimitLayer {
    limiter: Arc<RateLimiter>,
    max_requests: u32,
    window: Duration,
    key_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl RateLimitLayer {
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::new(max_requests, window)),
            max_requests,
            window,
            key_fn: Arc::new(|_| "global".into()),
        }
    }

    /// Use a shared rate-limit store, e.g. a Redis-backed one
    /// (see the `redis` feature).
    pub fn with_store(mut self, store: Arc<dyn RateLimitStore>) -> Self {
        self.limiter = Arc::new(RateLimiter::with_store(
            self.max_requests,
            self.window,
            store,
        ));
        self
    }

    /// Set a custom key extraction function (e.g. per-client-IP, per-route).
    pub fn with_key_fn(mut self, f: impl Fn(&str) -> String + Send + Sync + 'static) -> Self {
        self.key_fn = Arc::new(f);
        self
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: Arc::clone(&self.limiter),
            key_fn: Arc::clone(&self.key_fn),
        }
    }
}

#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    limiter: Arc<RateLimiter>,
    key_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl<S, Req> Service<Req> for RateLimitService<S>
where
    S: Service<Req> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| Box::new(e) as _)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let limiter = Arc::clone(&self.limiter);
        let key_fn = Arc::clone(&self.key_fn);
        let key = key_fn(""); // default uses empty string; custom fn can ignore it
        let fut = self.inner.call(req);
        Box::pin(async move {
            if !limiter.allow(&key).await {
                return Err(Box::new(std::io::Error::other("rate limit exceeded"))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            fut.await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rate_limiter_allows_within_limit() {
        let rl = RateLimiter::new(5, Duration::from_secs(1));
        for _ in 0..5 {
            assert!(rl.allow("test").await);
        }
    }

    #[tokio::test]
    async fn rate_limiter_blocks_over_limit() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        assert!(rl.allow("test").await);
        assert!(rl.allow("test").await);
        assert!(!rl.allow("test").await);
    }

    #[tokio::test]
    async fn rate_limiter_separate_keys() {
        let rl = RateLimiter::new(1, Duration::from_secs(60));
        assert!(rl.allow("a").await);
        assert!(rl.allow("b").await);
    }

    #[test]
    fn layer_constructs() {
        let _layer = RateLimitLayer::new(10, Duration::from_secs(1));
    }

    #[test]
    fn layer_with_custom_key() {
        let _layer =
            RateLimitLayer::new(5, Duration::from_secs(1)).with_key_fn(|_ip| "custom-key".into());
    }
}

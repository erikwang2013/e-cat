// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tower::{Layer, Service};

/// How often the in-memory store sweeps expired buckets.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Buckets idle for longer than this are removed by the sweep, which keeps
/// the map bounded by the distinct keys seen in the last day.
const MAX_IDLE: Duration = Duration::from_secs(24 * 60 * 60);

type KeyFn<B> = Arc<dyn Fn(&http::Request<B>) -> String + Send + Sync>;

#[async_trait]
pub trait RateLimitStore: Send + Sync {
    async fn check(&self, key: &str, max: u32, window_secs: u64) -> Result<(), String>;
}

struct MemoryStoreInner {
    buckets: HashMap<String, (u32, Instant)>,
    last_sweep: Instant,
}

pub struct MemoryStore {
    inner: Mutex<MemoryStoreInner>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MemoryStoreInner {
                buckets: HashMap::new(),
                last_sweep: Instant::now(),
            }),
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
        let mut inner = self.inner.lock().await;
        // Lazy expiry cleanup: periodically drop buckets that have been idle
        // longer than MAX_IDLE so the map cannot grow without bound.
        if inner.last_sweep.elapsed() >= SWEEP_INTERVAL {
            let cutoff = Instant::now() - MAX_IDLE;
            inner
                .buckets
                .retain(|_, (_, last_touched)| *last_touched > cutoff);
            inner.last_sweep = Instant::now();
        }
        let entry = inner
            .buckets
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

/// Default key extraction: first hop of `X-Forwarded-For`, falling back to
/// `X-Real-IP`, then to a shared `"global"` bucket when neither is present.
fn default_key_fn<B>(req: &http::Request<B>) -> String {
    for name in ["x-forwarded-for", "x-real-ip"] {
        if let Some(hop) = req
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next().map(str::trim))
            .filter(|hop| !hop.is_empty())
        {
            return hop.to_string();
        }
    }
    "global".into()
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

/// A rate-limiting tower layer.
///
/// The type parameter `B` is the request body type the key extraction
/// function inspects. `new` produces `RateLimitLayer<()>`; call
/// [`with_key_fn`](Self::with_key_fn) with the body type of your service
/// (e.g. `axum::body::Body`) to extract keys from the full request.
#[derive(Clone)]
pub struct RateLimitLayer<B = ()> {
    limiter: Arc<RateLimiter>,
    max_requests: u32,
    window: Duration,
    key_fn: KeyFn<B>,
}

impl RateLimitLayer<()> {
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::new(max_requests, window)),
            max_requests,
            window,
            key_fn: Arc::new(default_key_fn),
        }
    }
}

impl<B> RateLimitLayer<B> {
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

    /// Set a custom key extraction function that receives the full request
    /// (e.g. per-client-IP, per-route, per-user).
    ///
    /// The body type `B2` is inferred from the closure's argument, so write
    /// e.g. `|req: &http::Request<axum::body::Body>| ...`.
    pub fn with_key_fn<B2>(
        self,
        f: impl Fn(&http::Request<B2>) -> String + Send + Sync + 'static,
    ) -> RateLimitLayer<B2> {
        RateLimitLayer {
            limiter: self.limiter,
            max_requests: self.max_requests,
            window: self.window,
            key_fn: Arc::new(f),
        }
    }
}

impl<S, B> Layer<S> for RateLimitLayer<B>
where
    S: Service<http::Request<B>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    type Service = RateLimitService<S, B>;
    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: Arc::clone(&self.limiter),
            key_fn: Arc::clone(&self.key_fn),
        }
    }
}

#[derive(Clone)]
pub struct RateLimitService<S, B = ()> {
    inner: S,
    limiter: Arc<RateLimiter>,
    key_fn: KeyFn<B>,
}

impl<S, B> Service<http::Request<B>> for RateLimitService<S, B>
where
    S: Service<http::Request<B>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    B: Send + 'static,
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

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let limiter = Arc::clone(&self.limiter);
        let key_fn = Arc::clone(&self.key_fn);
        let key = key_fn(&req);
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
        let _layer = RateLimitLayer::new(5, Duration::from_secs(1))
            .with_key_fn(|_req: &http::Request<()>| "custom-key".into());
    }

    #[test]
    fn default_key_fn_uses_forwarded_for() {
        let req = http::Request::builder()
            .header("x-forwarded-for", "1.2.3.4, 5.6.7.8")
            .body(())
            .unwrap();
        assert_eq!(default_key_fn(&req), "1.2.3.4");
    }

    #[test]
    fn default_key_fn_uses_real_ip_fallback() {
        let req = http::Request::builder()
            .header("x-real-ip", "9.9.9.9")
            .body(())
            .unwrap();
        assert_eq!(default_key_fn(&req), "9.9.9.9");
    }

    #[test]
    fn default_key_fn_global_without_ip_headers() {
        let req = http::Request::builder().body(()).unwrap();
        assert_eq!(default_key_fn(&req), "global");
    }
}

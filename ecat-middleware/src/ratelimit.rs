// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::{Layer, Service};

/// Token-bucket rate limiter shared across clones.
struct RateLimiter {
    buckets: Mutex<HashMap<String, (u32, Instant)>>,
    max_requests: u32,
    window: Duration,
}

impl RateLimiter {
    fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            max_requests,
            window,
        }
    }

    fn allow(&self, key: &str) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();
        let entry = buckets.entry(key.to_string()).or_insert((0, now));
        if now.duration_since(entry.1) > self.window {
            *entry = (1, now);
            true
        } else if entry.0 < self.max_requests {
            entry.0 += 1;
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
pub struct RateLimitLayer {
    limiter: Arc<RateLimiter>,
}

impl RateLimitLayer {
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::new(max_requests, window)),
        }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: Arc::clone(&self.limiter),
        }
    }
}

#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    limiter: Arc<RateLimiter>,
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
        // Use a fixed key for all requests; callers can customize via extension
        if !self.limiter.allow("global") {
            return Box::pin(async {
                Err(Box::new(std::io::Error::other(
                    "rate limit exceeded",
                ))
                    as Box<dyn std::error::Error + Send + Sync>)
            });
        }
        let fut = self.inner.call(req);
        Box::pin(async move {
            fut.await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_within_limit() {
        let rl = RateLimiter::new(5, Duration::from_secs(1));
        for _ in 0..5 {
            assert!(rl.allow("test"));
        }
    }

    #[test]
    fn rate_limiter_blocks_over_limit() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        assert!(rl.allow("test"));
        assert!(rl.allow("test"));
        assert!(!rl.allow("test"));
    }

    #[test]
    fn rate_limiter_separate_keys() {
        let rl = RateLimiter::new(1, Duration::from_secs(60));
        assert!(rl.allow("a"));
        assert!(rl.allow("b")); // different key, not blocked
    }

    #[test]
    fn layer_constructs() {
        let _layer = RateLimitLayer::new(10, Duration::from_secs(1));
    }
}

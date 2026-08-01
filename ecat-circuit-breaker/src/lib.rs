// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::{Layer, Service};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}

struct SlidingWindow {
    successes: u64,
    failures: u64,
    window_start: Instant,
    window: Duration,
}

impl SlidingWindow {
    fn new(window: Duration) -> Self {
        Self {
            successes: 0,
            failures: 0,
            window_start: Instant::now(),
            window,
        }
    }

    fn record(&mut self, success: bool) {
        self.rotate();
        if success {
            self.successes += 1;
        } else {
            self.failures += 1;
        }
    }

    fn total(&mut self) -> u64 {
        self.rotate();
        self.successes + self.failures
    }

    fn failure_ratio(&mut self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        self.failures as f64 / total as f64
    }

    fn rotate(&mut self) {
        if self.window_start.elapsed() >= self.window {
            self.successes = 0;
            self.failures = 0;
            self.window_start = Instant::now();
        }
    }
}

struct BreakerInner {
    state: State,
    window: SlidingWindow,
    opened_at: Option<Instant>,
    half_open_count: u32,
}

#[derive(Clone)]
pub struct CircuitBreakerLayer {
    failure_ratio: f64,
    window: Duration,
    half_open_probes: u32,
    open_duration: Duration,
}

impl CircuitBreakerLayer {
    pub fn new() -> Self {
        Self {
            failure_ratio: 0.5,
            window: Duration::from_secs(30),
            half_open_probes: 3,
            open_duration: Duration::from_secs(10),
        }
    }

    pub fn failure_ratio(mut self, ratio: f64) -> Self {
        self.failure_ratio = ratio.clamp(0.0, 1.0);
        self
    }

    pub fn window(mut self, window: Duration) -> Self {
        self.window = window;
        self
    }

    pub fn half_open_probes(mut self, probes: u32) -> Self {
        self.half_open_probes = probes.max(1);
        self
    }

    pub fn open_duration(mut self, duration: Duration) -> Self {
        self.open_duration = duration;
        self
    }
}

impl Default for CircuitBreakerLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for CircuitBreakerLayer {
    type Service = CircuitBreakerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CircuitBreakerService {
            inner,
            breaker: Arc::new(Mutex::new(BreakerInner {
                state: State::Closed,
                window: SlidingWindow::new(self.window),
                opened_at: None,
                half_open_count: 0,
            })),
            config: Arc::new(self.clone()),
        }
    }
}

#[derive(Clone)]
pub struct CircuitBreakerService<S> {
    inner: S,
    breaker: Arc<Mutex<BreakerInner>>,
    config: Arc<CircuitBreakerLayer>,
}

impl<S, Req> Service<Req> for CircuitBreakerService<S>
where
    S: Service<Req> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::fmt::Display + std::error::Error + Send + Sync + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| Box::new(e) as _)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let mut breaker = self.breaker.lock().unwrap_or_else(|e| e.into_inner());
        let mut inner = self.inner.clone();
        let breaker_ref = Arc::clone(&self.breaker);
        let config = Arc::clone(&self.config);

        match breaker.state {
            State::Open => {
                if let Some(opened_at) = breaker.opened_at {
                    if opened_at.elapsed() >= config.open_duration {
                        tracing::info!("circuit breaker: open → half-open");
                        breaker.state = State::HalfOpen;
                        breaker.half_open_count = 0;
                    } else {
                        return Box::pin(async move {
                            Err(Box::new(std::io::Error::other("circuit breaker is open"))
                                as Box<dyn std::error::Error + Send + Sync>)
                        });
                    }
                }
            }
            State::HalfOpen => {
                if breaker.half_open_count >= config.half_open_probes {
                    return Box::pin(async move {
                        Err(
                            Box::new(std::io::Error::other("circuit breaker: too many probes"))
                                as Box<dyn std::error::Error + Send + Sync>,
                        )
                    });
                }
                breaker.half_open_count += 1;
            }
            State::Closed => {}
        }

        Box::pin(async move {
            let result = inner.call(req).await;
            let mut breaker = breaker_ref.lock().unwrap_or_else(|e| e.into_inner());

            match &result {
                Ok(_) => breaker.window.record(true),
                Err(e) => {
                    tracing::warn!(error = %e, "circuit breaker: request failed");
                    breaker.window.record(false);
                }
            }

            match breaker.state {
                State::Closed => {
                    if breaker.window.total() >= 5
                        && breaker.window.failure_ratio() >= config.failure_ratio
                    {
                        tracing::warn!(
                            ratio = breaker.window.failure_ratio(),
                            "circuit breaker: closed → open"
                        );
                        breaker.state = State::Open;
                        breaker.opened_at = Some(Instant::now());
                    }
                }
                State::HalfOpen => {
                    if result.is_ok() {
                        tracing::info!("circuit breaker: half-open → closed");
                        breaker.state = State::Closed;
                        breaker.opened_at = None;
                    } else {
                        tracing::warn!("circuit breaker: half-open → open (probe failed)");
                        breaker.state = State::Open;
                        breaker.opened_at = Some(Instant::now());
                    }
                }
                State::Open => {}
            }

            result.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_defaults() {
        let layer = CircuitBreakerLayer::new();
        assert!((layer.failure_ratio - 0.5).abs() < f64::EPSILON);
        assert_eq!(layer.window, Duration::from_secs(30));
        assert_eq!(layer.half_open_probes, 3);
    }

    #[test]
    fn layer_builder_methods() {
        let layer = CircuitBreakerLayer::new()
            .failure_ratio(0.8)
            .window(Duration::from_secs(10))
            .half_open_probes(5)
            .open_duration(Duration::from_secs(60));
        assert!((layer.failure_ratio - 0.8).abs() < f64::EPSILON);
        assert_eq!(layer.window, Duration::from_secs(10));
        assert_eq!(layer.half_open_probes, 5);
        assert_eq!(layer.open_duration, Duration::from_secs(60));
    }

    #[test]
    fn default_layer_construction() {
        let _layer = CircuitBreakerLayer::default();
    }

    #[test]
    fn sliding_window_counts() {
        let mut w = SlidingWindow::new(Duration::from_secs(60));
        assert_eq!(w.total(), 0);
        w.record(true);
        w.record(true);
        w.record(false);
        assert_eq!(w.total(), 3);
        assert!((w.failure_ratio() - 1.0 / 3.0).abs() < 0.01);
    }
}

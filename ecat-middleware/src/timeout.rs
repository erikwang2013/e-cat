use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tower::{Layer, Service};

#[derive(Clone)]
pub struct TimeoutLayer {
    timeout: Duration,
}

impl TimeoutLayer {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl<S> Layer<S> for TimeoutLayer {
    type Service = TimeoutService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        TimeoutService { inner, timeout: self.timeout }
    }
}

#[derive(Clone)]
pub struct TimeoutService<S> {
    inner: S,
    timeout: Duration,
}

impl<S, Req> Service<Req> for TimeoutService<S>
where
    S: Service<Req> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| Box::new(e) as _)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let fut = self.inner.call(req);
        let timeout = self.timeout;
        Box::pin(async move {
            tokio::time::timeout(timeout, fut)
                .await
                .map_err(|_| Box::new(std::io::Error::new(std::io::ErrorKind::TimedOut, "request timed out")) as Box<dyn std::error::Error + Send + Sync>)?
                .map_err(|e| Box::new(e) as _)
        })
    }
}

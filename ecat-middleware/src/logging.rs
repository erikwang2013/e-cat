// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::future::Future;
use std::pin::Pin;
use std::time::Instant;
use tower::{Layer, Service};

#[derive(Clone)]
pub struct LoggingLayer;

impl<S> Layer<S> for LoggingLayer {
    type Service = LoggingService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        LoggingService { inner }
    }
}

#[derive(Clone)]
pub struct LoggingService<S> {
    inner: S,
}

impl<S, Req> Service<Req> for LoggingService<S>
where
    S: Service<Req> + Send + 'static,
    S::Future: Send + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let start = Instant::now();
        let fut = self.inner.call(req);
        Box::pin(async move {
            let result = fut.await;
            let elapsed = start.elapsed();
            tracing::info!(
                duration_ms = elapsed.as_millis() as u64,
                "request completed"
            );
            result
        })
    }
}

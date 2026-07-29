use std::future::Future;
use std::pin::Pin;
use tower::{Layer, Service};

#[derive(Clone)]
pub struct TracingLayer;

impl<S> Layer<S> for TracingLayer {
    type Service = TracingService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        TracingService { inner }
    }
}

#[derive(Clone)]
pub struct TracingService<S> {
    inner: S,
}

impl<S, Req> Service<Req> for TracingService<S>
where
    S: Service<Req> + Send + 'static,
    S::Future: Send + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let span = tracing::info_span!("request");
        let _guard = span.enter();
        let fut = self.inner.call(req);
        Box::pin(async move { fut.await })
    }
}

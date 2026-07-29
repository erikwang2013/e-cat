use std::future::Future;
use std::pin::Pin;
use tower::{Layer, Service};

#[derive(Clone)]
pub struct RecoveryLayer;

impl<S> Layer<S> for RecoveryLayer {
    type Service = RecoveryService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RecoveryService { inner }
    }
}

#[derive(Clone)]
pub struct RecoveryService<S> {
    inner: S,
}

impl<S, Req> Service<Req> for RecoveryService<S>
where
    S: Service<Req> + Send + 'static,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
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
        Box::pin(async move {
            match tokio::task::spawn(fut).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(e)) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
                Err(_) => Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "task panicked")) as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

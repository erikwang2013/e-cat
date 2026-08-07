// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// Initialize structured logging with env filter.
///
/// NOTE: only one subscriber can be installed per process. Do not call this
/// together with `ecat_tracing_otlp::init` (or any other subscriber init);
/// the second `init` would panic with "a global default trace dispatcher
/// has already been set".
pub fn init(service_name: &str) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!(service = service_name, "tracing initialized");
}

/// Tower Layer that creates a request span with trace_id injection.
#[derive(Clone)]
pub struct TracingLayer {
    service_name: String,
}

impl TracingLayer {
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
        }
    }
}

impl<S> Layer<S> for TracingLayer {
    type Service = TracingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TracingService {
            inner,
            service_name: self.service_name.clone(),
        }
    }
}

#[derive(Clone)]
pub struct TracingService<S> {
    inner: S,
    service_name: String,
}

impl<S, Req> Service<Req> for TracingService<S>
where
    S: Service<Req> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| Box::new(e) as _)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        // Req 是完全泛型（仅 Send + 'static），无法在编译期读取请求头来填充
        // trace_id 字段，除非把 impl 特化为 http::Request<B>（会破坏泛型 API）。
        // 需要 trace_id 时请针对 http::Request<B> 的服务自行用
        // extract_trace_id()/inject_trace_id() 在调用处维护 span 字段。
        let span = tracing::info_span!(
            "request",
            service = %self.service_name,
        );
        let fut = self.inner.call(req);
        Box::pin(async move {
            let _guard = span.enter();
            fut.await.map_err(|e| Box::new(e) as _)
        })
    }
}

/// Extract trace_id from request headers for propagation.
///
/// Reads the canonical [`ecat_metadata::TRACE_ID`] header first, then
/// falls back to the W3C `traceparent` header.
pub fn extract_trace_id(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get(ecat_metadata::TRACE_ID)
        .or_else(|| headers.get("traceparent"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Inject trace_id into a header map for downstream calls.
///
/// Generates a random 32-hex-char trace id (UUID v4) under the canonical
/// [`ecat_metadata::TRACE_ID`] header.
pub fn inject_trace_id(headers: &mut http::HeaderMap) {
    let trace_id = uuid::Uuid::new_v4().simple().to_string();
    if let Ok(v) = http::HeaderValue::from_str(&trace_id) {
        headers.insert(ecat_metadata::TRACE_ID, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracing_layer_constructs() {
        let _layer = TracingLayer::new("test-service");
    }

    #[test]
    fn extract_empty_headers() {
        let headers = http::HeaderMap::new();
        assert_eq!(extract_trace_id(&headers), None);
    }

    #[test]
    fn extract_prefers_canonical_header_over_traceparent() {
        let mut headers = http::HeaderMap::new();
        headers.insert(ecat_metadata::TRACE_ID, "abc123".parse().unwrap());
        headers.insert("traceparent", "tp-000".parse().unwrap());
        assert_eq!(extract_trace_id(&headers).as_deref(), Some("abc123"));
    }

    #[test]
    fn extract_falls_back_to_traceparent() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            extract_trace_id(&headers).unwrap(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
    }

    #[test]
    fn inject_trace_id_adds_header() {
        let mut headers = http::HeaderMap::new();
        inject_trace_id(&mut headers);
        let value = headers
            .get(ecat_metadata::TRACE_ID)
            .expect("canonical header set");
        assert_eq!(value.len(), 32, "32-hex-char trace id");
        assert!(
            value.to_str().unwrap().chars().all(|c| c.is_ascii_hexdigit()),
            "trace id is hex"
        );
    }
}

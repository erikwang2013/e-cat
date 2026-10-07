// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use crate::{SecurityError, SecurityScanner, evaluate, request_parts};
use http::Request;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskCtx, Poll};

// ── Tower Layer ──

#[derive(Clone)]
pub struct SecurityLayer {
    scanner: Arc<SecurityScanner>,
}

impl SecurityLayer {
    pub fn new() -> Self {
        Self {
            scanner: Arc::new(SecurityScanner::new()),
        }
    }
}

impl Default for SecurityLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> tower::Layer<S> for SecurityLayer {
    type Service = SecurityService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityService {
            inner,
            scanner: Arc::clone(&self.scanner),
        }
    }
}

#[derive(Clone)]
pub struct SecurityService<S> {
    inner: S,
    scanner: Arc<SecurityScanner>,
}

impl<S, B> tower::Service<Request<B>> for SecurityService<S>
where
    S: tower::Service<Request<B>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    // 拦截时直接返回 403 响应而非 Err：Err 经服务端 no_error 归一到
    // Infallible 时触发 unreachable panic，会打崩 worker 线程
    S::Response: From<axum::response::Response> + Send,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = SecurityError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut TaskCtx<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready(cx)
            .map_err(|e| SecurityError::Inner(e.into()))
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let scanner = Arc::clone(&self.scanner);
        let parts = request_parts(&req);
        let strings: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
        let results = scanner.scan_parts(&strings);

        if let Some(err) = evaluate(&results) {
            use axum::response::IntoResponse;
            let resp: S::Response = err.into_response().into();
            return Box::pin(async move { Ok(resp) });
        }

        let fut = self.inner.call(req);
        Box::pin(async move { fut.await.map_err(|e| SecurityError::Inner(e.into())) })
    }
}

// ── Body-scanning variant ──

/// Tower layer that scans URI, headers, **and** the request body.
///
/// The body is read exactly once (up to [`body_limit`](Self::body_limit)
/// bytes) and passed through to the inner service, so handlers still receive
/// the full payload. Use this instead of [`SecurityLayer`] when request
/// bodies must also be checked for SQLi/XSS payloads.
#[derive(Clone)]
pub struct SecurityBodyLayer {
    scanner: Arc<SecurityScanner>,
    body_limit: usize,
}

impl SecurityBodyLayer {
    pub fn new() -> Self {
        Self {
            scanner: Arc::new(SecurityScanner::new()),
            body_limit: 10 * 1024 * 1024,
        }
    }

    /// Maximum body size (in bytes) that will be buffered and scanned.
    /// Larger bodies are rejected with a 413 (Payload Too Large) rather
    /// than buffered.
    pub fn body_limit(mut self, limit: usize) -> Self {
        self.body_limit = limit;
        self
    }
}

impl Default for SecurityBodyLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> tower::Layer<S> for SecurityBodyLayer {
    type Service = SecurityBodyService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityBodyService {
            inner,
            scanner: Arc::clone(&self.scanner),
            body_limit: self.body_limit,
        }
    }
}

#[derive(Clone)]
pub struct SecurityBodyService<S> {
    inner: S,
    scanner: Arc<SecurityScanner>,
    body_limit: usize,
}

impl<S> tower::Service<Request<axum::body::Body>> for SecurityBodyService<S>
where
    S: tower::Service<Request<axum::body::Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = S::Response;
    type Error = SecurityError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut TaskCtx<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready(cx)
            .map_err(|e| SecurityError::Inner(e.into()))
    }

    fn call(&mut self, req: Request<axum::body::Body>) -> Self::Future {
        let scanner = Arc::clone(&self.scanner);
        let body_limit = self.body_limit;
        let mut inner = self.inner.clone();
        let parts = request_parts(&req);
        let strings: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
        let header_results = scanner.scan_parts(&strings);

        Box::pin(async move {
            let (parts, body) = req.into_parts();
            // Read the body exactly once; the collected bytes become the new
            // body so downstream handlers can still access the payload.
            let bytes = match axum::body::to_bytes(body, body_limit).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    // LengthLimitError = 超限：413；其余读体错误为 500
                    let inner: Box<dyn std::error::Error + Send + Sync> = e.into_inner();
                    if inner
                        .downcast_ref::<http_body_util::LengthLimitError>()
                        .is_some()
                    {
                        return Err(SecurityError::BodyTooLarge);
                    }
                    return Err(SecurityError::Inner(inner));
                }
            };

            let mut results = header_results;
            results.extend(scanner.scan_body(&bytes));

            if let Some(err) = evaluate(&results) {
                return Err(err);
            }

            let req = Request::from_parts(parts, axum::body::Body::from(bytes));
            inner
                .call(req)
                .await
                .map_err(|e| SecurityError::Inner(e.into()))
        })
    }
}

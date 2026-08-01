// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use http::{Request, StatusCode};
use security_rust::Scanner;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskCtx, Poll};

pub use security_rust::{AttackCategory, DetectionResult, ScannerBuilder, Severity};

#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("attack blocked: {0}")]
    AttackBlocked(String),
    #[error("inner error: {0}")]
    Inner(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl SecurityError {
    pub fn to_http_status(&self) -> StatusCode {
        match self {
            Self::AttackBlocked(_) => StatusCode::FORBIDDEN,
            Self::Inner(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Wraps `security_rust::Scanner` with convenient constructors.
pub struct SecurityScanner {
    scanner: Scanner,
}

impl SecurityScanner {
    /// Create scanner with all 27 detectors enabled.
    pub fn new() -> Self {
        Self {
            scanner: Scanner::default(),
        }
    }

    /// Scan a single string through all detectors.
    pub fn scan(&self, input: &str) -> Vec<DetectionResult> {
        self.scanner.scan(input)
    }

    /// Scan multiple request parts (path, headers, body, etc.).
    pub fn scan_parts(&self, parts: &[&str]) -> Vec<DetectionResult> {
        let mut results = Vec::new();
        for part in parts {
            results.extend(self.scanner.scan(part));
        }
        results
    }
}

impl Default for SecurityScanner {
    fn default() -> Self {
        Self::new()
    }
}

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
        let mut parts: Vec<String> = Vec::new();
        parts.push(req.uri().to_string());

        for value in req.headers().values() {
            if let Ok(v) = value.to_str() {
                parts.push(v.to_string());
            }
        }

        let strings: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
        let results = self.scanner.scan_parts(&strings);

        for r in &results {
            tracing::warn!(
                attack_type = %r.attack_type,
                category = ?r.category,
                severity = ?r.severity,
                matched = %r.matched_pattern,
                "attack detected"
            );
        }

        if results
            .iter()
            .any(|r| matches!(r.severity, Severity::High | Severity::Critical))
        {
            let attack_types: Vec<String> =
                results.iter().map(|r| r.attack_type.to_string()).collect();
            return Box::pin(
                async move { Err(SecurityError::AttackBlocked(attack_types.join(", "))) },
            );
        }

        let fut = self.inner.call(req);
        Box::pin(async move { fut.await.map_err(|e| SecurityError::Inner(e.into())) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_detects_sql_injection() {
        let s = SecurityScanner::new();
        let results = s.scan("SELECT * FROM users; DROP TABLE users;");
        assert!(!results.is_empty());
    }

    #[test]
    fn scanner_detects_xss() {
        let s = SecurityScanner::new();
        let results = s.scan("<script>alert('xss')</script>");
        assert!(!results.is_empty());
    }

    #[test]
    fn scanner_clean_input_no_detection() {
        let s = SecurityScanner::new();
        let results = s.scan("hello world");
        assert!(results.is_empty());
    }

    #[test]
    fn scanner_scan_parts_aggregates() {
        let s = SecurityScanner::new();
        let results = s.scan_parts(&["clean", "<script>x</script>"]);
        assert!(!results.is_empty());
    }

    #[test]
    fn layer_constructs() {
        let _layer = SecurityLayer::new();
    }

    #[test]
    fn layer_default_constructs() {
        let _layer: SecurityLayer = Default::default();
    }
}

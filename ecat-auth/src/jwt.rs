// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::claims::AuthClaims;
use super::helpers::extract_bearer;
use http::{Request, Response, StatusCode};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// Errors while constructing or operating the JWT auth layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtAuthError {
    /// The shared secret is shorter than 32 bytes, which is too weak for HS256.
    WeakKey,
}

impl std::fmt::Display for JwtAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WeakKey => write!(f, "JWT secret must be at least 32 bytes for HS256"),
        }
    }
}

impl std::error::Error for JwtAuthError {}

enum JwtSecret {
    Shared(Vec<u8>),
    #[allow(dead_code)]
    RsaReserved(Vec<u8>),
}

#[derive(Clone)]
pub struct JwtAuthLayer {
    secret: Arc<JwtSecret>,
    required_claims: Vec<String>,
    header_name: String,
}

impl JwtAuthLayer {
    /// Create a layer for HS256-signed tokens.
    ///
    /// The secret must be at least 32 bytes (the minimum key size HS256
    /// accepts per RFC 7518); shorter keys are rejected with
    /// [`JwtAuthError::WeakKey`].
    pub fn new(secret: impl Into<String>) -> Result<Self, JwtAuthError> {
        let secret = secret.into();
        if secret.len() < 32 {
            return Err(JwtAuthError::WeakKey);
        }
        Ok(Self {
            secret: Arc::new(JwtSecret::Shared(secret.into_bytes())),
            required_claims: vec!["sub".into()],
            header_name: "Authorization".into(),
        })
    }

    pub fn require_claims(mut self, claims: &[&str]) -> Self {
        self.required_claims = claims.iter().map(|c| c.to_string()).collect();
        self
    }

    pub fn header_name(mut self, name: impl Into<String>) -> Self {
        self.header_name = name.into();
        self
    }
}

impl<S> Layer<S> for JwtAuthLayer {
    type Service = JwtAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        JwtAuthService {
            inner,
            config: Arc::new(self.clone()),
        }
    }
}

#[derive(Clone)]
pub struct JwtAuthService<S> {
    inner: S,
    config: Arc<JwtAuthLayer>,
}

impl<S, B> Service<Request<B>> for JwtAuthService<S>
where
    S: Service<Request<B>, Response = Response<axum::body::Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    B: Send + 'static,
{
    type Response = Response<axum::body::Body>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| Box::new(e) as _)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let token = extract_bearer(req.headers(), &self.config.header_name);
        let config = Arc::clone(&self.config);
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let token = match token {
                Some(t) => t,
                None => {
                    return Ok(Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(axum::body::Body::from(
                            r#"{"error":"missing authorization token"}"#,
                        ))
                        .unwrap());
                }
            };

            let secret_bytes = match config.secret.as_ref() {
                JwtSecret::Shared(b) => b,
                JwtSecret::RsaReserved(b) => b,
            };

            let validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
            let token_data = match jsonwebtoken::decode::<AuthClaims>(
                &token,
                &jsonwebtoken::DecodingKey::from_secret(secret_bytes),
                &validation,
            ) {
                Ok(data) => data,
                Err(e) => {
                    // Distinguish expiry in the logs without leaking
                    // jsonwebtoken internals to clients.
                    let expired =
                        matches!(e.kind(), jsonwebtoken::errors::ErrorKind::ExpiredSignature);
                    tracing::warn!(
                        error = %e,
                        expired,
                        "jwt validation failed"
                    );
                    return Ok(Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(axum::body::Body::from(r#"{"error":"invalid token"}"#))
                        .unwrap());
                }
            };

            for claim in &config.required_claims {
                let satisfied = match claim.as_str() {
                    "sub" => !token_data.claims.sub.is_empty(),
                    "role" => token_data.claims.role.is_some(),
                    _ => token_data.claims.extra.contains_key(claim),
                };
                if !satisfied {
                    return Ok(Response::builder()
                        .status(StatusCode::FORBIDDEN)
                        .body(axum::body::Body::from(format!(
                            r#"{{"error":"missing required claim: {claim}"}}"#
                        )))
                        .unwrap());
                }
            }

            let claims = token_data.claims;
            let mut req = req;
            req.extensions_mut().insert(claims);
            inner.call(req).await.map_err(|e| Box::new(e) as _)
        })
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use http::{HeaderMap, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

// ── Auth Claims ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthClaims {
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub exp: Option<u64>,
    #[serde(default)]
    pub iat: Option<u64>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl AuthClaims {
    pub fn subject(&self) -> &str {
        &self.sub
    }

    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.role.as_deref() == Some(role)
    }
}

// ── JWT Auth Layer ──

#[derive(Clone)]
pub struct JwtAuthLayer {
    secret: Arc<JwtSecret>,
    required_claims: Vec<String>,
    header_name: String,
}

enum JwtSecret {
    Shared(Vec<u8>),
    #[allow(dead_code)]
    Rsa(Vec<u8>),
}

impl JwtAuthLayer {
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: Arc::new(JwtSecret::Shared(secret.into().into_bytes())),
            required_claims: vec!["sub".into()],
            header_name: "Authorization".into(),
        }
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
                JwtSecret::Rsa(b) => b,
            };

            let validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
            let token_data = match jsonwebtoken::decode::<AuthClaims>(
                &token,
                &jsonwebtoken::DecodingKey::from_secret(secret_bytes),
                &validation,
            ) {
                Ok(data) => data,
                Err(e) => {
                    tracing::warn!(error = %e, "jwt validation failed");
                    return Ok(Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(axum::body::Body::from(format!(
                            r#"{{"error":"invalid token: {e}"}}"#
                        )))
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

// ── API Key Layer ──

#[derive(Clone)]
pub struct ApiKeyLayer {
    keys: Arc<HashMap<String, AuthClaims>>,
    header_name: String,
    query_param: Option<String>,
}

impl ApiKeyLayer {
    pub fn new(keys: HashMap<String, AuthClaims>) -> Self {
        Self {
            keys: Arc::new(keys),
            header_name: "X-API-Key".into(),
            query_param: None,
        }
    }

    pub fn header_name(mut self, name: impl Into<String>) -> Self {
        self.header_name = name.into();
        self
    }

    pub fn query_param(mut self, param: impl Into<String>) -> Self {
        self.query_param = Some(param.into());
        self
    }
}

impl<S> Layer<S> for ApiKeyLayer {
    type Service = ApiKeyService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ApiKeyService {
            inner,
            config: Arc::new(self.clone()),
        }
    }
}

#[derive(Clone)]
pub struct ApiKeyService<S> {
    inner: S,
    config: Arc<ApiKeyLayer>,
}

impl<S, B> Service<Request<B>> for ApiKeyService<S>
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
        let key = extract_header(req.headers(), &self.config.header_name).or_else(|| {
            self.config
                .query_param
                .as_ref()
                .and_then(|p| extract_query_param(req.uri().query(), p))
        });

        let claims = key.and_then(|k| self.config.keys.get(&k).cloned());
        let mut inner = self.inner.clone();

        Box::pin(async move {
            match claims {
                Some(c) => {
                    let mut req = req;
                    req.extensions_mut().insert(c);
                    inner.call(req).await.map_err(|e| Box::new(e) as _)
                }
                None => Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(axum::body::Body::from(r#"{"error":"invalid api key"}"#))
                    .unwrap()),
            }
        })
    }
}

// ── Extractors ──

pub fn claims_from_request<B>(req: &Request<B>) -> Option<&AuthClaims> {
    req.extensions().get::<AuthClaims>()
}

// ── Helpers ──

fn extract_bearer(headers: &HeaderMap, header_name: &str) -> Option<String> {
    let value = headers.get(header_name)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

fn extract_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(|v| v.to_string())
}

fn extract_query_param(query: Option<&str>, param: &str) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == param {
            return parts.next().map(|v| v.to_string());
        }
    }
    None
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn bearer_extraction() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_static("Bearer mytoken123"),
        );
        assert_eq!(
            extract_bearer(&headers, "Authorization"),
            Some("mytoken123".into())
        );
    }

    #[test]
    fn bearer_extraction_no_header() {
        let headers = HeaderMap::new();
        assert_eq!(extract_bearer(&headers, "Authorization"), None);
    }

    #[test]
    fn bearer_extraction_wrong_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_static("Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        );
        assert_eq!(extract_bearer(&headers, "Authorization"), None);
    }

    #[test]
    fn query_param_extraction() {
        assert_eq!(
            extract_query_param(Some("key=abc123&other=val"), "key"),
            Some("abc123".into())
        );
    }

    #[test]
    fn query_param_not_found() {
        assert_eq!(extract_query_param(Some("a=1&b=2"), "c"), None);
    }

    #[test]
    fn layer_construction() {
        let _layer = JwtAuthLayer::new("secret-key")
            .require_claims(&["sub", "role"])
            .header_name("X-Auth-Token");
    }

    #[test]
    fn api_key_layer_construction() {
        let mut keys = HashMap::new();
        keys.insert(
            "key1".into(),
            AuthClaims {
                sub: "user1".into(),
                exp: None,
                iat: None,
                role: Some("admin".into()),
                extra: HashMap::new(),
            },
        );
        let _layer = ApiKeyLayer::new(keys).query_param("api_key");
    }

    #[test]
    fn claims_subject_and_role() {
        let claims = AuthClaims {
            sub: "user42".into(),
            exp: None,
            iat: None,
            role: Some("editor".into()),
            extra: HashMap::new(),
        };
        assert_eq!(claims.subject(), "user42");
        assert!(claims.has_role("editor"));
        assert!(!claims.has_role("admin"));
    }
}

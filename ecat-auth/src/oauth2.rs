// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::claims::AuthClaims;
use super::helpers::extract_bearer;
use http::{Request, Response, StatusCode};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

#[derive(Clone)]
pub struct OAuth2Layer {
    introspection_url: String,
    client_id: String,
    client_secret: String,
    cache_ttl_secs: u64,
}

impl OAuth2Layer {
    pub fn new(
        introspection_url: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            introspection_url: introspection_url.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            cache_ttl_secs: 300,
        }
    }

    pub fn cache_ttl(mut self, secs: u64) -> Self {
        self.cache_ttl_secs = secs;
        self
    }
}

impl<S> Layer<S> for OAuth2Layer {
    type Service = OAuth2Service<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OAuth2Service {
            inner,
            config: Arc::new(self.clone()),
        }
    }
}

#[derive(Clone)]
pub struct OAuth2Service<S> {
    inner: S,
    config: Arc<OAuth2Layer>,
}

impl<S, B> Service<Request<B>> for OAuth2Service<S>
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
        let token = extract_bearer(req.headers(), "Authorization");
        let config = Arc::clone(&self.config);
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let token = match token {
                Some(t) => t,
                None => {
                    return Ok(Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(axum::body::Body::from(
                            r#"{"error":"missing bearer token"}"#,
                        ))
                        .unwrap());
                }
            };

            match introspect_token(&config, &token).await {
                Ok(c) => {
                    let mut req = req;
                    req.extensions_mut().insert(c);
                    inner.call(req).await.map_err(|e| Box::new(e) as _)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "oauth2 introspection failed");
                    Ok(Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(axum::body::Body::from(format!(
                            r#"{{"error":"invalid token: {e}"}}"#
                        )))
                        .unwrap())
                }
            }
        })
    }
}

async fn introspect_token(config: &OAuth2Layer, token: &str) -> Result<AuthClaims, String> {
    let client = reqwest::Client::new();
    let params = [
        ("token", token),
        ("client_id", &config.client_id),
        ("client_secret", &config.client_secret),
    ];

    let resp = client
        .post(&config.introspection_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("introspection request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("introspection returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("introspection parse: {e}"))?;

    let active = body
        .get("active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !active {
        return Err("token is not active".into());
    }

    let sub = body
        .get("sub")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let role = body
        .get("role")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut extra = HashMap::new();
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            if !matches!(
                k.as_str(),
                "active" | "sub" | "role" | "client_id" | "exp" | "iat"
            ) {
                extra.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(AuthClaims {
        sub,
        exp: body.get("exp").and_then(|v| v.as_u64()),
        iat: body.get("iat").and_then(|v| v.as_u64()),
        role,
        extra,
    })
}

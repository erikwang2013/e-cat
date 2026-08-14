// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::claims::AuthClaims;
use super::helpers::extract_bearer;
use http::{Request, Response, StatusCode};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// HTTP timeout for token introspection requests.
const INTROSPECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 内省缓存容量上限：达到后按 FIFO 逐出最旧条目，防止海量唯一 token
/// 无限占用内存（S2 DoS）。
const CACHE_CAPACITY: usize = 10_000;

/// 内省结果缓存：token -> (claims, 缓存时间)。TTL 内命中直接返回 claims
/// （避免每请求反序列化 JSON），过期后重新 introspection。
/// FIFO 有界：达到容量上限时逐出最旧条目；order 与 entries 一一对应，
/// 每个 key 只入队一次。
struct IntrospectCache {
    entries: HashMap<String, (AuthClaims, std::time::Instant)>,
    order: VecDeque<String>,
}

impl IntrospectCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

#[derive(Clone)]
pub struct OAuth2Layer {
    introspection_url: String,
    client_id: String,
    client_secret: String,
    cache_ttl_secs: u64,
    cache_capacity: usize,
    /// Shared HTTP client: connections are pooled and reused across requests
    /// instead of being created (and torn down) per request.
    client: reqwest::Client,
    cache: Arc<tokio::sync::RwLock<IntrospectCache>>,
}

impl OAuth2Layer {
    /// The introspection URL must use `https`; plain `http` is rejected
    /// (skipped in `cfg(test)` so unit tests may point at a local server).
    pub fn new(
        introspection_url: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Result<Self, String> {
        let introspection_url = introspection_url.into();
        #[cfg(not(test))]
        {
            if !introspection_url.starts_with("https://") {
                return Err(format!(
                    "introspection URL must use https, got: {introspection_url}"
                ));
            }
        }
        let client = reqwest::Client::builder()
            .timeout(INTROSPECT_TIMEOUT)
            .build()
            .map_err(|e| format!("failed to build http client: {e}"))?;
        Ok(Self {
            introspection_url,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            cache_ttl_secs: 300,
            cache_capacity: CACHE_CAPACITY,
            client,
            cache: Arc::new(tokio::sync::RwLock::new(IntrospectCache::new())),
        })
    }

    pub fn cache_ttl(mut self, secs: u64) -> Self {
        self.cache_ttl_secs = secs;
        self
    }

    pub fn cache_capacity(mut self, n: usize) -> Self {
        self.cache_capacity = n.max(1);
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
                        .body(axum::body::Body::from(r#"{"error":"invalid token"}"#))
                        .unwrap())
                }
            }
        })
    }
}

async fn introspect_token(config: &OAuth2Layer, token: &str) -> Result<AuthClaims, String> {
    // TTL 内命中缓存，避免每个请求都打 introspection 端点。
    if config.cache_ttl_secs > 0 {
        let cache = config.cache.read().await;
        if let Some((claims, cached_at)) = cache.entries.get(token)
            && cached_at.elapsed() < std::time::Duration::from_secs(config.cache_ttl_secs)
        {
            return Ok(claims.clone());
        }
    }

    let params = [
        ("token", token),
        ("client_id", &config.client_id),
        ("client_secret", &config.client_secret),
    ];

    let resp = config
        .client
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

    let claims = AuthClaims {
        sub,
        exp: body.get("exp").and_then(|v| v.as_u64()),
        iat: body.get("iat").and_then(|v| v.as_u64()),
        role,
        extra,
    };

    if config.cache_ttl_secs > 0 {
        let mut cache = config.cache.write().await;
        // 新 key 且容量已满：FIFO 逐出最旧条目（order 与 entries 一一对应，
        // 每个 key 只入队一次，不产生重复条目）。
        if !cache.entries.contains_key(token) {
            if cache.entries.len() >= config.cache_capacity
                && let Some(oldest) = cache.order.pop_front()
            {
                cache.entries.remove(&oldest);
            }
            cache.order.push_back(token.to_string());
        }
        cache.entries.insert(
            token.to_string(),
            (claims.clone(), std::time::Instant::now()),
        );
    }

    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn spawn_introspection_server(
        count: &'static AtomicUsize,
    ) -> String {
        use axum::Json;
        use axum::response::IntoResponse;
        let app = axum::Router::new().route(
            "/introspect",
            post(move || async move {
                count.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "active": true,
                    "sub": "user-1",
                    "role": "admin",
                    "exp": 9999999999u64,
                }))
                .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/introspect")
    }

    #[tokio::test]
    async fn introspection_cached_within_ttl() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let url = spawn_introspection_server(&COUNT).await;
        let cfg = OAuth2Layer::new(url, "cid", "csecret")
            .unwrap()
            .cache_ttl(60);

        let claims1 = introspect_token(&cfg, "tok-1").await.unwrap();
        let claims2 = introspect_token(&cfg, "tok-1").await.unwrap();
        assert_eq!(claims1.sub, "user-1");
        assert_eq!(claims2.sub, "user-1");
        assert_eq!(COUNT.load(Ordering::SeqCst), 1, "second call hits cache");

        let claims3 = introspect_token(&cfg, "tok-2").await.unwrap();
        assert_eq!(claims3.role.as_deref(), Some("admin"));
        assert_eq!(COUNT.load(Ordering::SeqCst), 2, "new token re-introspects");
    }

    /// S2 回归：缓存必须被容量上限约束。容量满后按 FIFO 逐出最旧条目，
    /// 海量唯一 token 不会让缓存无限增长。
    #[tokio::test]
    async fn cache_evicts_oldest_at_capacity() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let url = spawn_introspection_server(&COUNT).await;
        let cfg = OAuth2Layer::new(url, "cid", "csecret")
            .unwrap()
            .cache_ttl(3600)
            .cache_capacity(3);

        for token in ["tok-1", "tok-2", "tok-3", "tok-4"] {
            introspect_token(&cfg, token).await.unwrap();
        }
        assert_eq!(COUNT.load(Ordering::SeqCst), 4);

        // 容量 3：tok-1 最先被逐出，缓存大小不超过上限。
        {
            let cache = cfg.cache.read().await;
            assert_eq!(cache.entries.len(), 3);
            assert!(!cache.entries.contains_key("tok-1"));
            assert!(cache.entries.contains_key("tok-2"));
        }

        // 被逐出的 tok-1 需重新 introspection；随后 tok-2 按 FIFO 被挤出。
        introspect_token(&cfg, "tok-1").await.unwrap();
        assert_eq!(COUNT.load(Ordering::SeqCst), 5);
        assert!(!cfg.cache.read().await.entries.contains_key("tok-2"));
    }

    /// P1 优化：缓存直接保存 AuthClaims 结构体，命中路径不再每请求
    /// serde_json 反序列化。
    #[tokio::test]
    async fn cache_stores_claims_without_json_roundtrip() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let url = spawn_introspection_server(&COUNT).await;
        let cfg = OAuth2Layer::new(url, "cid", "csecret")
            .unwrap()
            .cache_ttl(60);

        introspect_token(&cfg, "tok-1").await.unwrap();
        let cache = cfg.cache.read().await;
        let (claims, _) = cache.entries.get("tok-1").expect("token cached");
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.role.as_deref(), Some("admin"));
    }

    #[tokio::test]
    async fn introspection_ttl_zero_disables_cache() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let url = spawn_introspection_server(&COUNT).await;
        let cfg = OAuth2Layer::new(url, "cid", "csecret")
            .unwrap()
            .cache_ttl(0);

        let _ = introspect_token(&cfg, "tok-1").await.unwrap();
        let _ = introspect_token(&cfg, "tok-1").await.unwrap();
        assert_eq!(
            COUNT.load(Ordering::SeqCst),
            2,
            "ttl=0 must never cache"
        );
    }
}

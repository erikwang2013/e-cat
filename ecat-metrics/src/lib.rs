// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use prometheus::{Encoder, Registry, TextEncoder};
use std::sync::OnceLock;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

pub fn metrics_text() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    if encoder.encode(&registry().gather(), &mut buffer).is_err() {
        return String::from("# metrics encoding failed\n");
    }
    String::from_utf8(buffer).unwrap_or_else(|_| String::from("# metrics: invalid utf-8\n"))
}

pub fn metrics_router() -> Router {
    async fn handler() -> impl IntoResponse {
        (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            metrics_text(),
        )
    }
    Router::new().route("/metrics", get(handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_singleton() {
        let r1 = registry() as *const Registry;
        let r2 = registry() as *const Registry;
        assert_eq!(r1, r2);
    }

    #[test]
    fn metrics_text_does_not_panic() {
        let text = metrics_text();
        // empty registry produces empty or minimal output — just check it's valid UTF-8
        let _ = text;
    }

    #[tokio::test]
    async fn metrics_router_serves_prometheus_text() {
        use tower::ServiceExt;
        let router = metrics_router();
        let resp = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/plain"), "got content-type: {ct}");
    }
}

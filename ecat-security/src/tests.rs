// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
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
fn attack_blocked_maps_to_403() {
    use axum::response::IntoResponse;
    let resp = SecurityError::AttackBlocked("sqli".into()).into_response();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[test]
fn inner_error_maps_to_500() {
    use axum::response::IntoResponse;
    let resp = SecurityError::Inner(Box::new(std::io::Error::other("boom"))).into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn layer_constructs() {
    let _layer = SecurityLayer::new();
}

#[test]
fn layer_default_constructs() {
    let _layer: SecurityLayer = Default::default();
}

#[test]
fn body_layer_constructs() {
    let _layer = SecurityBodyLayer::new().body_limit(1024);
}

#[test]
fn body_layer_default_constructs() {
    let _layer: SecurityBodyLayer = Default::default();
}

#[tokio::test]
async fn body_layer_blocks_attack_in_body() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityBodyLayer::new();
    let svc = layer.layer(tower::service_fn(|_: Request<axum::body::Body>| async {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::empty()))
    }));

    let req = http::Request::builder()
        .method("POST")
        .uri("/submit")
        .body(axum::body::Body::from("<script>alert('xss')</script>"))
        .unwrap();
    let result = svc.oneshot(req).await;
    assert!(matches!(result, Err(SecurityError::AttackBlocked(_))));
}

#[tokio::test]
async fn body_over_limit_maps_to_413() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityBodyLayer::new().body_limit(8);
    let svc = layer.layer(tower::service_fn(|_: Request<axum::body::Body>| async {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::empty()))
    }));

    let req = http::Request::builder()
        .method("POST")
        .uri("/submit")
        .body(axum::body::Body::from("x".repeat(64)))
        .unwrap();
    let result = svc.oneshot(req).await;
    assert!(matches!(result, Err(SecurityError::BodyTooLarge)));
    assert_eq!(
        SecurityError::BodyTooLarge.to_http_status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn inner_error_response_body_does_not_leak_detail() {
    use axum::response::IntoResponse;
    let resp = SecurityError::Inner(Box::new(std::io::Error::other(
        "secret-db-dsn=s3://user:pass",
    )))
    .into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let (_, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 4096).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("secret-db-dsn"), "leaked detail: {text}");
    assert!(text.contains("internal server error"), "got: {text}");
}

#[tokio::test]
async fn attack_blocked_response_body_shape() {
    use axum::response::IntoResponse;
    let resp = SecurityError::AttackBlocked("sqli, xss".into()).into_response();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let (_, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1024).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(
        text,
        r#"{"error":"attack blocked","types":"sqli, xss"}"#.to_string()
    );
}

#[tokio::test]
async fn body_too_large_response_body_shape() {
    use axum::response::IntoResponse;
    let resp = SecurityError::BodyTooLarge.into_response();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let (_, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1024).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text, r#"{"error":"request body too large"}"#.to_string());
}

#[test]
fn request_parts_include_uri_and_headers() {
    let req = http::Request::builder()
        .uri("/search?q=abc")
        .header("X-Custom", "val-1")
        .body(())
        .unwrap();
    let parts = request_parts(&req);
    assert_eq!(
        parts,
        vec!["/search?q=abc".to_string(), "val-1".to_string()]
    );
}

#[test]
fn percent_decode_handles_encoded_sql_chars() {
    assert_eq!(
        percent_decode("/q?x=SELECT%20*%20FROM%20users"),
        "/q?x=SELECT * FROM users"
    );
    assert_eq!(percent_decode("1%27%20OR%20%271%27%3D%271"), "1' OR '1'='1");
    assert_eq!(
        percent_decode("/clean?q=hello%20world"),
        "/clean?q=hello world"
    );
    assert_eq!(percent_decode("%3cscript%3e"), "<script>");
    // 无效 % 序列原样保留
    assert_eq!(percent_decode("/%zz%"), "/%zz%");
    assert_eq!(percent_decode("/100%25"), "/100%");
}

#[test]
fn request_parts_percent_decode_uri_only() {
    let req = http::Request::builder()
        .uri("/search?q=SELECT%20*%20FROM%20users")
        .header("X-Custom", "val%201")
        .body(())
        .unwrap();
    let parts = request_parts(&req);
    // URI 解码后进入扫描列表；header 原样保留（header 无编码层）
    assert_eq!(parts[0], "/search?q=SELECT * FROM users");
    assert_eq!(parts[1], "val%201");
}

#[tokio::test]
async fn header_layer_blocks_attack_in_uri() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityLayer::new();
    let svc = layer.layer(tower::service_fn(|_: Request<()>| async {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::empty()))
    }));

    // URI 不允许空格，SQLi 正则需字面空白 → 用 URI 合法字符即可命中的
    // javascript: XSS 载荷
    let req = http::Request::builder()
        .uri("/redirect?url=javascript:alert(1)")
        .body(())
        .unwrap();
    let resp = svc.oneshot(req).await.expect("blocked as response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn header_layer_blocks_attack_in_header() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityLayer::new();
    let svc = layer.layer(tower::service_fn(|_: Request<()>| async {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::empty()))
    }));

    let req = http::Request::builder()
        .uri("/clean")
        .header("X-Trace", "<script>alert(1)</script>")
        .body(())
        .unwrap();
    let resp = svc.oneshot(req).await.expect("blocked as response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn header_layer_blocks_encoded_sqli_in_uri() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityLayer::new();
    let svc = layer.layer(tower::service_fn(|_: Request<()>| async {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::empty()))
    }));

    // %20 编码空格：解码后 `SELECT * FROM users` 命中 SQLi 正则
    let req = http::Request::builder()
        .uri("/search?q=SELECT%20*%20FROM%20users")
        .body(())
        .unwrap();
    let resp = svc.oneshot(req).await.expect("blocked as response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn header_layer_blocks_encoded_single_quote_sqli_in_uri() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityLayer::new();
    let svc = layer.layer(tower::service_fn(|_: Request<()>| async {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::empty()))
    }));

    // %27 编码单引号 + %20 编码空格：解码后 `1' OR '1'='1` 命中 SQLi 正则
    let req = http::Request::builder()
        .uri("/login?u=1%27%20OR%20%271%27%3D%271")
        .body(())
        .unwrap();
    let resp = svc.oneshot(req).await.expect("blocked as response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn header_layer_passes_clean_encoded_query_through() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityLayer::new();
    let svc = layer.layer(tower::service_fn(|req: Request<()>| async move {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::from(
            req.uri().path().to_string(),
        )))
    }));

    // 正常请求含 %20 编码空格不应误拦截
    let req = http::Request::builder()
        .uri("/search?q=hello%20world")
        .body(())
        .unwrap();
    let resp = svc.oneshot(req).await.expect("clean request passes");
    let (_, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1024).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&bytes), "/search");
}

#[tokio::test]
async fn header_layer_passes_proxy_headers_with_internal_ip() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityLayer::new();
    let svc = layer.layer(tower::service_fn(|req: Request<()>| async move {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::from(
            req.uri().path().to_string(),
        )))
    }));

    // nginx 网关注入的代理头携带 docker 内网 IP，不应触发 SSRF 拦截
    let req = http::Request::builder()
        .uri("/api/v1/booking/dates?region_id=1")
        .header("X-Real-IP", "172.19.0.1")
        .header("X-Forwarded-For", "172.19.0.1")
        .body(())
        .unwrap();
    let resp = svc.oneshot(req).await.expect("proxy headers pass");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn header_layer_passes_clean_request_through() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityLayer::new();
    let svc = layer.layer(tower::service_fn(|req: Request<()>| async move {
        Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::from(
            req.uri().path().to_string(),
        )))
    }));

    let req = http::Request::builder().uri("/clean").body(()).unwrap();
    let resp = svc.oneshot(req).await.expect("clean request passes");
    let (_, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1024).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&bytes), "/clean");
}

#[tokio::test]
async fn body_layer_passes_clean_body_through() {
    use tower::Layer as _;
    use tower::ServiceExt;

    let layer = SecurityBodyLayer::new();
    let svc = layer.layer(tower::service_fn(
        |req: Request<axum::body::Body>| async move {
            let (_, body) = req.into_parts();
            let bytes = axum::body::to_bytes(body, 1024).await.unwrap();
            Ok::<_, std::convert::Infallible>(http::Response::new(axum::body::Body::from(bytes)))
        },
    ));

    let req = http::Request::builder()
        .method("POST")
        .uri("/submit")
        .body(axum::body::Body::from("hello world"))
        .unwrap();
    let resp = svc.oneshot(req).await.expect("clean body passes through");
    let (_, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1024).await.unwrap();
    assert_eq!(&bytes[..], b"hello world");
}

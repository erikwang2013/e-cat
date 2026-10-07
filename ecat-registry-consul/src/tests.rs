// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::*;

#[test]
fn consul_registry_constructs() {
    let _reg = ConsulRegistry::new("http://localhost:8500")
        .unwrap()
        .datacenter("dc2")
        .token("secret-token");
}

#[test]
fn consul_registry_clone() {
    let reg = ConsulRegistry::new("http://localhost:8500").unwrap();
    let _reg2 = reg.clone();
}

#[test]
fn new_rejects_insecure_remote_url() {
    let err = ConsulRegistry::new("http://consul.internal:8500").unwrap_err();
    assert!(err.contains("https"), "got: {err}");
    assert!(ConsulRegistry::new("https://consul.internal:8500").is_ok());
}

#[test]
fn new_accepts_loopback_http_for_dev() {
    assert!(ConsulRegistry::new("http://127.0.0.1:8500").is_ok());
    assert!(ConsulRegistry::new("http://[::1]:8500").is_ok());
    assert!(ConsulRegistry::new("http://localhost:8500").is_ok());
}

#[test]
fn is_loopback_detects_dev_hosts() {
    assert!(is_loopback_host("localhost"));
    assert!(is_loopback_host("127.0.0.1"));
    assert!(is_loopback_host("127.8.8.8"));
    assert!(is_loopback_host("::1"));
    assert!(!is_loopback_host("10.0.0.5"));
    assert!(!is_loopback_host("consul.internal"));
}

#[test]
fn extract_host_ipv4() {
    assert_eq!(extract_host("http://10.0.0.5:8080"), "10.0.0.5");
    assert_eq!(extract_host("https://example.com:443"), "example.com");
    assert_eq!(extract_host("http://10.0.0.5:8080/health"), "10.0.0.5");
}

#[test]
fn extract_host_ipv6_literal() {
    assert_eq!(extract_host("http://[::1]:8080"), "::1");
    assert_eq!(extract_host("https://[2001:db8::1]:443"), "2001:db8::1");
    assert_eq!(extract_host("http://[::1]"), "::1");
}

#[test]
fn extract_port_ipv6() {
    assert_eq!(extract_port("http://[::1]:8080"), Some(8080));
    assert_eq!(extract_port("http://[::1]:8080/health"), Some(8080));
    assert_eq!(extract_port("http://[::1]"), None);
}

/// mock Consul 的 /v1/health/service/<name> 端点，返回给定 JSON 文本。
async fn spawn_mock_health(body: &'static str) -> String {
    let app = axum::Router::new().route(
        "/v1/health/service/{name}",
        axum::routing::get(move || async move {
            axum::response::Response::new(axum::body::Body::from(body))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn discover_uses_https_tag_and_brackets_ipv6() {
    let body = r#"[
            {"Node":{"Address":"10.0.0.5"},
             "Service":{"Service":"web","Address":"2001:db8::1","Port":8443,"Tags":["version=2.0","https"]}}
        ]"#;
    let base_url = spawn_mock_health(body).await;
    let reg = ConsulRegistry::new(base_url).unwrap();
    let services = reg.discover("web").await.unwrap();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].endpoints, vec!["https://[2001:db8::1]:8443"]);
    assert_eq!(services[0].version, "2.0");
}

#[tokio::test]
async fn discover_defaults_to_http() {
    let body = r#"[
            {"Node":{"Address":"10.0.0.5"},
             "Service":{"Service":"api","Address":"10.0.0.9","Port":9000,"Tags":[]}}
        ]"#;
    let base_url = spawn_mock_health(body).await;
    let reg = ConsulRegistry::new(base_url).unwrap();
    let services = reg.discover("api").await.unwrap();
    assert_eq!(services[0].endpoints, vec!["http://10.0.0.9:9000"]);
}

#[tokio::test]
async fn register_sends_health_check() {
    let seen = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/v1/agent/service/register",
        axum::routing::put(
            move |axum::Json(body): axum::Json<serde_json::Value>| async move {
                *s.lock().unwrap() = Some(body);
                axum::http::StatusCode::OK
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}")).unwrap();
    let service = ServiceInfo::new("web", "1.0").with_endpoint("http://10.0.0.5:8080");
    let _registration = reg.register(service).await.unwrap();

    let body = seen.lock().unwrap().take().expect("register body");
    let check = body.get("Check").expect("health check");
    assert_eq!(check["HTTP"], "http://10.0.0.5:8080/health");
    assert_eq!(check["Interval"], "10s");
    assert_eq!(check["Timeout"], "3s");
}

#[test]
fn percent_encode_segment_encodes_reserved_chars() {
    assert_eq!(percent_encode_segment("api/v2"), "api%2Fv2");
    assert_eq!(percent_encode_segment("my dc"), "my%20dc");
    // 非 ASCII 按 UTF-8 字节逐字节编码
    assert_eq!(percent_encode_segment("服务"), "%E6%9C%8D%E5%8A%A1");
}

/// 捕获 PUT /v1/agent/service/register 请求体的 mock，返回 (base_url, 捕获体)。
async fn spawn_register_capture() -> (String, Arc<std::sync::Mutex<Option<serde_json::Value>>>) {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/v1/agent/service/register",
        axum::routing::put(
            move |axum::Json(body): axum::Json<serde_json::Value>| async move {
                *s.lock().unwrap() = Some(body);
                axum::http::StatusCode::OK
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn register_without_endpoint_uses_default_address_and_no_check() {
    let (base_url, seen) = spawn_register_capture().await;
    let reg = ConsulRegistry::new(base_url).unwrap();
    reg.register(ServiceInfo::new("web", "1.0")).await.unwrap();
    let body = seen.lock().unwrap().take().expect("register body");
    assert_eq!(body["Address"], "127.0.0.1");
    assert!(body.get("Port").is_none(), "no port → no health check");
    assert!(body.get("Check").is_none());
    assert_eq!(body["Name"], "web");
}

#[tokio::test]
async fn register_https_endpoint_probes_https() {
    let (base_url, seen) = spawn_register_capture().await;
    let reg = ConsulRegistry::new(base_url).unwrap();
    let service = ServiceInfo::new("web", "1.0").with_endpoint("https://10.0.0.5:8443");
    reg.register(service).await.unwrap();
    let body = seen.lock().unwrap().take().expect("register body");
    assert_eq!(body["Address"], "10.0.0.5");
    assert_eq!(body["Port"], 8443);
    assert_eq!(body["Check"]["HTTP"], "https://10.0.0.5:8443/health");
}

#[tokio::test]
async fn register_ipv6_endpoint_brackets_host() {
    let (base_url, seen) = spawn_register_capture().await;
    let reg = ConsulRegistry::new(base_url).unwrap();
    let service = ServiceInfo::new("web", "1.0").with_endpoint("http://[2001:db8::1]:8080");
    reg.register(service).await.unwrap();
    let body = seen.lock().unwrap().take().expect("register body");
    assert_eq!(body["Address"], "2001:db8::1");
    assert_eq!(body["Check"]["HTTP"], "http://[2001:db8::1]:8080/health");
}

#[tokio::test]
async fn register_error_response_is_err() {
    let app = axum::Router::new().route(
        "/v1/agent/service/register",
        axum::routing::put(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}")).unwrap();
    let err = match reg.register(ServiceInfo::new("web", "1.0")).await {
        Ok(_) => panic!("register must fail on 500"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("register failed"), "got: {err}");
}

#[tokio::test]
async fn deregister_passes_id_in_path() {
    let seen = Arc::new(std::sync::Mutex::new(None::<String>));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/v1/agent/service/deregister/{id}",
        axum::routing::put(
            move |axum::extract::Path(id): axum::extract::Path<String>| async move {
                *s.lock().unwrap() = Some(id);
                axum::http::StatusCode::OK
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}")).unwrap();
    reg.deregister("web-uuid").await.unwrap();
    assert_eq!(seen.lock().unwrap().as_deref(), Some("web-uuid"));
}

#[tokio::test]
async fn token_header_sent_when_configured() {
    let seen = Arc::new(std::sync::Mutex::new(None::<String>));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/v1/agent/services",
        axum::routing::get(move |headers: axum::http::HeaderMap| async move {
            if let Some(tok) = headers.get("x-consul-token") {
                *s.lock().unwrap() = Some(tok.to_str().unwrap().to_string());
            }
            axum::Json(serde_json::json!({}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}"))
        .unwrap()
        .token("tok-1");
    assert!(reg.list_services().await.unwrap().is_empty());
    assert_eq!(seen.lock().unwrap().as_deref(), Some("tok-1"));
}

#[tokio::test]
async fn token_header_absent_when_not_configured() {
    let seen = Arc::new(std::sync::Mutex::new(false));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/v1/agent/services",
        axum::routing::get(move |headers: axum::http::HeaderMap| async move {
            if headers.contains_key("x-consul-token") {
                *s.lock().unwrap() = true;
            }
            axum::Json(serde_json::json!({}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}")).unwrap();
    assert!(reg.list_services().await.unwrap().is_empty());
    assert!(
        !*seen.lock().unwrap(),
        "token must not be sent without config"
    );
}

#[tokio::test]
async fn list_services_parses_agent_services_map() {
    let app = axum::Router::new().route(
        "/v1/agent/services",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "id1": {"Service": "auth"},
                "id2": {"Service": "gw"},
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}")).unwrap();
    let names = reg.list_services().await.unwrap();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"auth".to_string()));
    assert!(names.contains(&"gw".to_string()));
}

#[tokio::test]
async fn discover_falls_back_to_node_address() {
    let body = r#"[
            {"Node":{"Address":"10.1.2.3"},
             "Service":{"Service":"api","Port":9000,"Tags":[]}}
        ]"#;
    let base_url = spawn_mock_health(body).await;
    let reg = ConsulRegistry::new(base_url).unwrap();
    let services = reg.discover("api").await.unwrap();
    assert_eq!(services[0].endpoints, vec!["http://10.1.2.3:9000"]);
}

#[tokio::test]
async fn discover_empty_result_is_ok() {
    let base_url = spawn_mock_health("[]").await;
    let reg = ConsulRegistry::new(base_url).unwrap();
    assert!(reg.discover("nope").await.unwrap().is_empty());
}

#[tokio::test]
async fn discover_error_response_is_err() {
    let app = axum::Router::new().route(
        "/v1/health/service/{name}",
        axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}")).unwrap();
    let err = reg.discover("web").await.unwrap_err();
    assert!(err.to_string().contains("discover failed"), "got: {err}");
}

#[tokio::test]
async fn discover_percent_encodes_service_name_and_dc() {
    let seen = Arc::new(std::sync::Mutex::new(None::<String>));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/v1/health/service/{name}",
        axum::routing::get(
            move |axum::extract::RawQuery(q): axum::extract::RawQuery| async move {
                *s.lock().unwrap() = q;
                axum::Json(serde_json::json!([]))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let reg = ConsulRegistry::new(format!("http://{addr}"))
        .unwrap()
        .datacenter("my dc");
    reg.discover("api/v2").await.unwrap();
    let guard = seen.lock().unwrap();
    let q = guard.as_deref().unwrap_or_default();
    assert!(q.contains("dc=my%20dc"), "got: {q}");
    assert!(q.contains("passing=true"), "got: {q}");
}

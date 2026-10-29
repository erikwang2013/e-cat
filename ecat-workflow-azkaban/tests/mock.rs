// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Azkaban 适配器对 axum mock 的行为测试：登录/session、create、触发/查询/
//! 取消/列运行、错误映射、配置 flatten、超时与熔断。不依赖真实 Azkaban。

use axum::Json;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::SET_COOKIE;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ecat_workflow::{
    RunFilter, RunId, RunState, WorkflowClient, WorkflowError, WorkflowId, WorkflowSpec,
};
use ecat_workflow_azkaban::{AzkabanClient, AzkabanConfig};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Mock {
    logins: AtomicUsize,
    business_hits: AtomicUsize,
    expire_once: AtomicBool,
    cookie_login: AtomicBool,
    slow: AtomicBool,
    fail_status: AtomicU16,
    body_override: Mutex<Option<String>>,
    seen: Mutex<Vec<String>>,
}

fn has(mock: &Mock, needle: &str) -> bool {
    mock.seen
        .lock()
        .unwrap()
        .iter()
        .any(|entry| entry.contains(needle))
}

fn seen_len(mock: &Mock) -> usize {
    mock.seen.lock().unwrap().len()
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn client_from(config: Value) -> AzkabanClient {
    AzkabanClient::from_config(serde_json::from_value(config).unwrap()).unwrap()
}

fn client(url: &str, project: &str) -> AzkabanClient {
    client_from(json!({
        "base_url": url,
        "username": "user",
        "password": "pass",
        "project": project,
    }))
}

async fn mock_handler(State(mock): State<Arc<Mock>>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body).to_string();
    mock.seen
        .lock()
        .unwrap()
        .push(format!("{method} {uri} {body}"));
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or_default().to_string();

    if path == "/" && query.contains("action=login") {
        let n = mock.logins.fetch_add(1, Ordering::SeqCst) + 1;
        if mock.cookie_login.load(Ordering::SeqCst) {
            return (
                [(SET_COOKIE, "azkaban.browser.session.id=cookie-sess; Path=/")],
                "{}",
            )
                .into_response();
        }
        return Json(json!({"session.id": format!("sess-{n}")})).into_response();
    }
    if path == "/manager" && query.contains("ajax=upload") {
        return Json(json!({"status": "success"})).into_response();
    }
    if path == "/manager" && query.contains("action=create") {
        return Json(json!({"status": "success"})).into_response();
    }
    if path == "/executor" && query.contains("ajax=executeFlow") {
        if mock.slow.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        if mock.expire_once.swap(false, Ordering::SeqCst) {
            return (StatusCode::UNAUTHORIZED, "session expired").into_response();
        }
        return Json(json!({"execid": "42"})).into_response();
    }
    if path == "/executor" && query.contains("ajax=fetchexecflow") {
        mock.business_hits.fetch_add(1, Ordering::SeqCst);
        let code = mock.fail_status.load(Ordering::SeqCst);
        if code != 0 {
            return (StatusCode::from_u16(code).unwrap(), "boom").into_response();
        }
        if let Some(body) = mock.body_override.lock().unwrap().clone() {
            return body.into_response();
        }
        return Json(json!({
            "project": "p",
            "flow": "f",
            "status": "SUCCEEDED",
            "startTime": 1_700_000_000_000u64,
            "endTime": 0,
        }))
        .into_response();
    }
    if path == "/executor" && query.contains("ajax=cancelFlow") {
        return Json(json!({})).into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected").into_response()
}

fn spec(name: &str, definition: Value, params: Value) -> WorkflowSpec {
    WorkflowSpec {
        name: name.to_string(),
        definition,
        params,
        schedule: None,
    }
}

fn zip_b64() -> String {
    STANDARD.encode(b"PK\x03\x04payload")
}

#[tokio::test]
async fn core_operations_hit_documented_endpoints() {
    let mock = Arc::new(Mock::default());
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client(&url, "p");

    let id = client
        .create(&spec("f", json!(zip_b64()), Value::Null))
        .await
        .unwrap();
    assert_eq!(id, WorkflowId("p/f".into()));
    assert!(has(&mock, "/manager?action=create"));
    assert!(has(&mock, "session.id=sess-1"));
    assert!(has(&mock, "filename=\"f.zip\""));
    assert!(has(&mock, "application/zip"));

    let run = client.trigger(&id, json!({"k": "v"})).await.unwrap();
    assert_eq!(run, RunId("42".into()));
    assert!(has(&mock, "ajax=executeFlow"));
    assert!(has(&mock, "flowOverride%5Bk%5D=v"));

    let status = client.status(&run).await.unwrap();
    assert_eq!(status.state, RunState::Succeeded);
    assert_eq!(status.workflow_id, WorkflowId("p/f".into()));
    assert_eq!(status.started_at.as_deref(), Some("1700000000000"));
    assert_eq!(status.finished_at, None);

    client.cancel(&run).await.unwrap();
    assert!(has(&mock, "ajax=cancelFlow"));

    assert_eq!(mock.logins.load(Ordering::SeqCst), 1, "session 应被复用");
}

#[tokio::test]
async fn expired_session_relogins_and_retries_once() {
    let mock = Arc::new(Mock::default());
    mock.expire_once.store(true, Ordering::SeqCst);
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client(&url, "p");

    let run = client
        .trigger(&WorkflowId("p/f".into()), Value::Null)
        .await
        .unwrap();
    assert_eq!(run, RunId("42".into()));
    assert_eq!(mock.logins.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn login_falls_back_to_set_cookie_session() {
    let mock = Arc::new(Mock::default());
    mock.cookie_login.store(true, Ordering::SeqCst);
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client(&url, "p");

    client
        .trigger(&WorkflowId("p/f".into()), Value::Null)
        .await
        .unwrap();
    assert!(has(&mock, "session.id=cookie-sess"));
}

#[tokio::test]
async fn list_runs_is_unsupported() {
    let client = client("http://127.0.0.1:1", "p");
    assert!(!client.capabilities().list_runs);
    let err = client.list_runs(&RunFilter::default()).await.unwrap_err();
    assert!(matches!(err, WorkflowError::Unsupported(_)), "{err:?}");
}

#[tokio::test]
async fn non_2xx_maps_to_http_error() {
    let mock = Arc::new(Mock::default());
    mock.fail_status.store(500, Ordering::SeqCst);
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client(&url, "p");

    let err = client.status(&RunId("1".into())).await.unwrap_err();
    assert!(
        matches!(err, WorkflowError::Http { status: 500, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn invalid_json_maps_to_parse_error() {
    let mock = Arc::new(Mock::default());
    *mock.body_override.lock().unwrap() = Some("not json".into());
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client(&url, "p");

    let err = client.status(&RunId("1".into())).await.unwrap_err();
    assert!(matches!(err, WorkflowError::Parse(_)), "{err:?}");
}

#[tokio::test]
async fn local_validation_errors_need_no_network() {
    let client = client("http://127.0.0.1:1", "p");

    let err = client
        .create(&spec("f", Value::Null, Value::Null))
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::Config(_)), "{err:?}");

    let err = client
        .create(&spec("f", json!({"zip_base64": "!!!"}), Value::Null))
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::Config(_)), "{err:?}");

    let err = client
        .trigger(&WorkflowId("p/f".into()), json!([1, 2]))
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::Unsupported(_)), "{err:?}");

    let err = client
        .trigger(&WorkflowId("bad-id".into()), Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::Config(_)), "{err:?}");
}

#[test]
fn config_flattens_http_and_engine_fields() {
    let cfg: AzkabanConfig = serde_json::from_value(json!({
        "base_url": "http://azkaban:8081",
        "username": "u",
        "password": "p",
        "project": "proj",
        "query_timeout_secs": 7,
        "breaker": {"failure_ratio": 0.9, "window": 60},
    }))
    .unwrap();
    assert_eq!(cfg.http.base_url, "http://azkaban:8081");
    assert_eq!(cfg.http.username.as_deref(), Some("u"));
    assert_eq!(cfg.http.query_timeout_secs, Some(7));
    assert_eq!(cfg.project.as_deref(), Some("proj"));
    assert_eq!(cfg.http.breaker.as_ref().unwrap().failure_ratio, 0.9);
}

#[tokio::test]
async fn slow_backend_maps_to_timeout() {
    let mock = Arc::new(Mock::default());
    mock.slow.store(true, Ordering::SeqCst);
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client_from(json!({
        "base_url": url,
        "username": "user",
        "password": "pass",
        "query_timeout_secs": 1,
    }));

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.trigger(&WorkflowId("p/f".into()), Value::Null),
    )
    .await
    .expect("guarded 应在 1s 后返回，而不是挂起");
    assert!(
        matches!(result, Err(WorkflowError::Timeout(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn repeated_failures_open_the_breaker() {
    let mock = Arc::new(Mock::default());
    mock.fail_status.store(500, Ordering::SeqCst);
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client(&url, "p");
    let run = RunId("1".into());

    // 首次 status 触发登录（1 次成功）后连续失败；第 5 次总调用时
    // 失败率 4/5 ≥ 0.5，熔断器打开。
    for _ in 0..4 {
        let err = client.status(&run).await.unwrap_err();
        assert!(
            matches!(err, WorkflowError::Http { status: 500, .. }),
            "{err:?}"
        );
    }
    let err = client.status(&run).await.unwrap_err();
    assert!(
        matches!(err, WorkflowError::Unavailable(_)),
        "熔断器应已打开: {err:?}"
    );
    assert_eq!(
        mock.business_hits.load(Ordering::SeqCst),
        4,
        "打开后的调用不应打到后端"
    );
    assert_eq!(mock.logins.load(Ordering::SeqCst), 1);
    assert_eq!(seen_len(&mock), 5, "登录 1 次 + 业务 4 次 + 第 5 次未出站");
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Oozie 适配器对 axum mock 的行为测试：提交/启动/查询/取消/列运行、
//! XML 参数注入与转义、错误映射、配置 flatten、超时与熔断。不依赖真实 Oozie。

use axum::Json;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ecat_workflow::{
    RunFilter, RunId, RunState, WorkflowClient, WorkflowError, WorkflowId, WorkflowSpec,
};
use ecat_workflow_oozie::{OozieClient, OozieConfig};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const JOB: &str = "0000001-261029100000000-oozie-erik-W";

#[derive(Default)]
struct Mock {
    hits: AtomicUsize,
    slow: AtomicBool,
    fail_status: AtomicU16,
    submit_body: Mutex<Option<String>>,
    submit_content_type: Mutex<Option<String>>,
    status_body: Mutex<Option<String>>,
    auth: Mutex<Vec<String>>,
    seen: Mutex<Vec<String>>,
}

fn has(mock: &Mock, needle: &str) -> bool {
    mock.seen
        .lock()
        .unwrap()
        .iter()
        .any(|entry| entry.contains(needle))
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn client_from(config: Value) -> OozieClient {
    OozieClient::from_config(serde_json::from_value(config).unwrap()).unwrap()
}

fn client(url: &str) -> OozieClient {
    client_from(json!({
        "base_url": url,
        "username": "user",
        "password": "pass",
    }))
}

async fn mock_handler(State(mock): State<Arc<Mock>>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(auth) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
    {
        mock.auth.lock().unwrap().push(auth.to_string());
    }
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

    if path == "/oozie/v2/jobs" && method == "POST" {
        *mock.submit_content_type.lock().unwrap() = content_type;
        *mock.submit_body.lock().unwrap() = Some(body);
        return (StatusCode::CREATED, Json(json!({"id": JOB}))).into_response();
    }
    if path == "/oozie/v2/jobs" && method == "GET" {
        mock.hits.fetch_add(1, Ordering::SeqCst);
        return Json(json!({"workflows": [
            {"id": "job-2", "status": "RUNNING", "startTime": "2026-10-29T10:00:00Z"},
            {"id": "job-3", "status": "FAILED", "errorMessage": "boom"}
        ]}))
        .into_response();
    }
    if path.starts_with("/oozie/v2/job/") {
        if query.contains("action=start") || query.contains("action=kill") {
            return StatusCode::OK.into_response();
        }
        if query.contains("show=status") {
            mock.hits.fetch_add(1, Ordering::SeqCst);
            if mock.slow.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let code = mock.fail_status.load(Ordering::SeqCst);
            if code != 0 {
                return (StatusCode::from_u16(code).unwrap(), "boom").into_response();
            }
            if let Some(body) = mock.status_body.lock().unwrap().clone() {
                return body.into_response();
            }
            // show=status 只返回状态；id/时间缺失时适配器回退到 RunId。
            return Json(json!({"status": "SUCCEEDED"})).into_response();
        }
    }
    (StatusCode::NOT_FOUND, "unexpected").into_response()
}

fn spec(definition: Value, params: Value) -> WorkflowSpec {
    WorkflowSpec {
        name: "wf".into(),
        definition,
        params,
        schedule: None,
    }
}

fn definition() -> String {
    "<?xml version=\"1.0\"?><configuration>\
     <property><name>oozie.wf.application.path</name><value>/apps/wf</value></property>\
     </configuration>"
        .to_string()
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
    let client = client(&url);

    let id = client
        .create(&spec(json!(definition()), json!({"k": "v<&"})))
        .await
        .unwrap();
    assert_eq!(id, WorkflowId(JOB.into()));
    assert_eq!(
        mock.submit_content_type.lock().unwrap().as_deref(),
        Some("application/xml")
    );
    let submitted = mock.submit_body.lock().unwrap().clone().unwrap();
    assert!(
        submitted.contains(
            "<property><name>k</name><value>v&lt;&amp;</value></property></configuration>"
        ),
        "params 应转义后注入在闭合标签前: {submitted}"
    );
    assert_eq!(
        mock.auth.lock().unwrap().first().map(String::as_str),
        Some("Basic dXNlcjpwYXNz"),
        "Oozie 使用 Basic auth"
    );

    let run = client.trigger(&id, Value::Null).await.unwrap();
    assert_eq!(run, RunId(JOB.into()));
    assert!(has(&mock, &format!("PUT /oozie/v2/job/{JOB}?action=start")));

    let status = client.status(&run).await.unwrap();
    assert_eq!(status.state, RunState::Succeeded);
    assert_eq!(status.workflow_id, WorkflowId(JOB.into()));
    assert_eq!(status.started_at, None);
    assert_eq!(status.finished_at, None);
    assert!(has(&mock, "show=status"));

    client.cancel(&run).await.unwrap();
    assert!(has(&mock, &format!("PUT /oozie/v2/job/{JOB}?action=kill")));

    let runs = client
        .list_runs(&RunFilter {
            workflow_id: None,
            state: None,
            limit: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].run_id, RunId("job-2".into()));
    assert_eq!(runs[0].state, RunState::Running);
    assert_eq!(runs[1].state, RunState::Failed);
    assert_eq!(runs[1].message.as_deref(), Some("boom"));
    assert!(has(&mock, "GET /oozie/v2/jobs?jobtype=wf&len=2"));

    let filtered = client
        .list_runs(&RunFilter {
            workflow_id: Some(WorkflowId("job-3".into())),
            state: Some(RunState::Failed),
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].run_id, RunId("job-3".into()));
    assert!(has(&mock, "filter=id%3Djob-3"));
}

#[tokio::test]
async fn trigger_params_and_bad_ids_are_local_errors() {
    let client = client("http://127.0.0.1:1");

    let err = client
        .trigger(&WorkflowId(JOB.into()), json!({"k": "v"}))
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::Unsupported(_)), "{err:?}");

    let err = client
        .trigger(&WorkflowId("../evil".into()), Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::Config(_)), "{err:?}");

    let err = client.status(&RunId("bad id".into())).await.unwrap_err();
    assert!(matches!(err, WorkflowError::Config(_)), "{err:?}");
}

#[tokio::test]
async fn definition_and_params_validation() {
    let client = client("http://127.0.0.1:1");

    for definition in [
        Value::Null,
        json!({"yaml": "<configuration/>"}),
        json!({"xml": "<configuration><property></configuration>"}),
        json!("not xml at all"),
    ] {
        let err = client
            .create(&spec(definition.clone(), Value::Null))
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkflowError::Config(_) | WorkflowError::Parse(_)),
            "definition {definition}: {err:?}"
        );
    }

    let err = client
        .create(&spec(json!(definition()), json!([1, 2])))
        .await
        .unwrap_err();
    assert!(matches!(err, WorkflowError::Unsupported(_)), "{err:?}");

    let err = client
        .create(&spec(json!(definition()), Value::Null))
        .await
        .unwrap_err();
    assert!(
        matches!(err, WorkflowError::Transport(_)),
        "合法 XML 应走到网络层: {err:?}"
    );
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
    let client = client(&url);

    let err = client.status(&RunId(JOB.into())).await.unwrap_err();
    assert!(
        matches!(err, WorkflowError::Http { status: 500, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn invalid_json_maps_to_parse_error() {
    let mock = Arc::new(Mock::default());
    *mock.status_body.lock().unwrap() = Some("not json".into());
    let url = serve(
        Router::new()
            .fallback(mock_handler)
            .with_state(mock.clone()),
    )
    .await;
    let client = client(&url);

    let err = client.status(&RunId(JOB.into())).await.unwrap_err();
    assert!(matches!(err, WorkflowError::Parse(_)), "{err:?}");
}

#[test]
fn config_flattens_http_fields() {
    let cfg: OozieConfig = serde_json::from_value(json!({
        "base_url": "http://oozie:11000",
        "username": "u",
        "password": "p",
        "query_timeout_secs": 7,
        "breaker": {"failure_ratio": 0.9, "window": 60},
    }))
    .unwrap();
    assert_eq!(cfg.http.base_url, "http://oozie:11000");
    assert_eq!(cfg.http.username.as_deref(), Some("u"));
    assert_eq!(cfg.http.query_timeout_secs, Some(7));
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

    let result = tokio::time::timeout(Duration::from_secs(10), client.status(&RunId(JOB.into())))
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
    let client = client(&url);
    let run = RunId(JOB.into());

    // 无登录步骤：5 次全失败，第 5 次记录后熔断器打开。
    for _ in 0..5 {
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
        mock.hits.load(Ordering::SeqCst),
        5,
        "打开后的调用不应打到后端"
    );
}

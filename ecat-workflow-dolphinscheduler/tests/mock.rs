// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! DolphinScheduler 适配器对 axum mock 的行为测试：form 编码、triggerCode 轮询、
//! 状态映射、token 优先级、错误映射、配置 flatten、超时与熔断。不依赖真实 DS。

use axum::Json;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use ecat_workflow::{
    RunFilter, RunId, RunState, WorkflowClient, WorkflowError, WorkflowId, WorkflowSpec,
};
use ecat_workflow_dolphinscheduler::{DolphinSchedulerClient, DolphinSchedulerConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROJECT: &str = "1234567890123";
const API: &str = "/dolphinscheduler";

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn client_from(config: Value) -> DolphinSchedulerClient {
    DolphinSchedulerClient::from_config(serde_json::from_value(config).unwrap()).unwrap()
}

fn base(url: &str) -> String {
    format!("{url}{API}")
}

// ── mock handlers ──────────────────────────────────────────

async fn ds_start(Form(form): Form<HashMap<String, String>>) -> Response {
    let ok = form.get("processDefinitionCode").map(String::as_str) == Some("42")
        && form.get("failureStrategy").map(String::as_str) == Some("CONTINUE")
        && form.get("warningType").map(String::as_str) == Some("NONE")
        && form.get("startParams").map(String::as_str) == Some(r#"{"k":"v"}"#)
        && form.get("scheduleTime").is_some_and(|t| t.len() == 19)
        && form.get("tenantCode").map(String::as_str) == Some("tenant-a");
    if !ok {
        return (StatusCode::BAD_REQUEST, format!("bad form: {form:?}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": 987654321})).into_response()
}

async fn ds_poll(
    State(polls): State<Arc<AtomicUsize>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if query.get("triggerCode").map(String::as_str) != Some("987654321") {
        return (StatusCode::BAD_REQUEST, format!("bad query: {query:?}")).into_response();
    }
    if polls.fetch_add(1, Ordering::SeqCst) == 0 {
        return Json(json!({"code": 0, "msg": "success", "data": []})).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": [{
        "id": 777,
        "processDefinitionCode": 42,
        "state": "RUNNING_EXECUTION",
        "startTime": "2026-10-29 10:00:00",
        "endTime": null,
    }]}))
    .into_response()
}

async fn ds_status(Path(id): Path<String>) -> Response {
    if id != "777" {
        return (StatusCode::BAD_REQUEST, format!("bad instance id: {id}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": {
        "id": 777,
        "processDefinitionCode": 42,
        "state": "SUCCESS",
        "startTime": "2026-10-29 10:00:00",
        "endTime": "2026-10-29 10:05:00",
    }}))
    .into_response()
}

async fn ds_cancel(Form(form): Form<HashMap<String, String>>) -> Response {
    if form.get("processInstanceId").map(String::as_str) != Some("777")
        || form.get("executeType").map(String::as_str) != Some("STOP")
    {
        return (StatusCode::BAD_REQUEST, format!("bad form: {form:?}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": null})).into_response()
}

async fn ds_list(Query(query): Query<HashMap<String, String>>) -> Response {
    let ok = query.get("pageNo").map(String::as_str) == Some("1")
        && query.get("pageSize").map(String::as_str) == Some("3")
        && query.get("processDefineCode").map(String::as_str) == Some("42")
        && query.get("stateType").map(String::as_str) == Some("FAILURE");
    if !ok {
        return (StatusCode::BAD_REQUEST, format!("bad query: {query:?}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": {
        "totalList": [{
            "id": 778,
            "processDefinitionCode": 42,
            "state": "FAILURE",
            "startTime": "2026-10-29 11:00:00",
            "endTime": "2026-10-29 11:01:00",
        }],
        "total": 1,
        "totalPage": 1,
        "currentPage": 1,
    }}))
    .into_response()
}

async fn ds_create(Form(form): Form<HashMap<String, String>>) -> Response {
    let expected = [
        ("name", "etl"),
        ("description", "daily etl"),
        ("tenantCode", "tenant-a"),
        ("executionType", "PARALLEL"),
        ("taskDefinitionJson", r#"[{"code":1}]"#),
        ("taskRelationJson", r#"[{"x":1}]"#),
        ("locations", r#"[{"taskCode":1,"x":0,"y":0}]"#),
    ];
    for (key, value) in expected {
        if form.get(key).map(String::as_str) != Some(value) {
            return (StatusCode::BAD_REQUEST, format!("bad {key}: {form:?}")).into_response();
        }
    }
    (
        StatusCode::CREATED,
        Json(json!({"code": 0, "msg": "success", "data": {
            "code": 123456789012345_i64,
            "name": "etl",
        }})),
    )
        .into_response()
}

async fn ds_token(Path(_id): Path<String>, headers: HeaderMap) -> Response {
    let tokens: Vec<&str> = headers
        .get_all("token")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if tokens != ["field-value"] {
        return (
            StatusCode::UNAUTHORIZED,
            format!("token headers: {tokens:?}"),
        )
            .into_response();
    }
    if headers.get("x-extra").and_then(|v| v.to_str().ok()) != Some("kept") {
        return (StatusCode::BAD_REQUEST, "x-extra header lost").into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": {
        "id": 9,
        "processDefinitionCode": 42,
        "state": "RUNNING_EXECUTION",
    }}))
    .into_response()
}

// ── 用例 ───────────────────────────────────────────────────

#[tokio::test]
async fn trigger_polls_trigger_code_then_status_cancel_and_list_work() {
    let polls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .nest(
            API,
            Router::new()
                .route(
                    format!("/projects/{PROJECT}/executors/start-process-instance").as_str(),
                    post(ds_start),
                )
                .route(
                    format!("/projects/{PROJECT}/process-instances/trigger").as_str(),
                    get(ds_poll),
                )
                .route(
                    format!("/projects/{PROJECT}/process-instances/{{id}}").as_str(),
                    get(ds_status),
                )
                .route(
                    format!("/projects/{PROJECT}/executors/execute").as_str(),
                    post(ds_cancel),
                )
                .route(
                    format!("/projects/{PROJECT}/process-instances").as_str(),
                    get(ds_list),
                ),
        )
        .with_state(Arc::clone(&polls));
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": base(&url),
        "project_code": 1234567890123_i64,
        "tenant_code": "tenant-a",
        "api_style": "legacy",
    }));

    assert_eq!(client.engine(), "dolphinscheduler");
    let caps = client.capabilities();
    assert!(caps.create && caps.trigger && caps.cancel && caps.list_runs);

    let run = client
        .trigger(&WorkflowId("42".into()), json!({"k": "v"}))
        .await
        .expect("trigger");
    assert_eq!(run, RunId("777".into()));
    assert_eq!(
        polls.load(Ordering::SeqCst),
        2,
        "第一次轮询为空，第二次才有实例"
    );

    let status = client.status(&run).await.expect("status");
    assert_eq!(status.state, RunState::Succeeded);
    assert_eq!(status.workflow_id, WorkflowId("42".into()));
    assert_eq!(status.run_id, run);
    assert_eq!(status.started_at.as_deref(), Some("2026-10-29 10:00:00"));
    assert_eq!(status.finished_at.as_deref(), Some("2026-10-29 10:05:00"));

    client.cancel(&run).await.expect("cancel");

    let runs = client
        .list_runs(&RunFilter {
            workflow_id: Some(WorkflowId("42".into())),
            state: Some(RunState::Failed),
            limit: Some(3),
        })
        .await
        .expect("list");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Failed);
    assert_eq!(runs[0].run_id, RunId("778".into()));
}

#[tokio::test]
async fn create_posts_form_fields_and_returns_definition_code() {
    let app = Router::new().nest(
        API,
        Router::new().route(
            format!("/projects/{PROJECT}/process-definition").as_str(),
            post(ds_create),
        ),
    );
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": base(&url),
        "project_code": 1234567890123_i64,
        "tenant_code": "tenant-a",
        "api_style": "legacy",
    }));

    let id = client
        .create(&WorkflowSpec {
            name: "etl".into(),
            definition: json!({
                "description": "daily etl",
                "taskDefinitionJson": [{"code": 1}],
                "taskRelationJson": r#"[{"x":1}]"#,
                "locations": [{"taskCode": 1, "x": 0, "y": 0}],
            }),
            params: Value::Null,
            schedule: None,
        })
        .await
        .expect("create");
    assert_eq!(id, WorkflowId("123456789012345".into()));
}

#[tokio::test]
async fn create_without_task_definition_json_is_config_error() {
    let client = client_from(json!({
        "base_url": "http://127.0.0.1:1",
        "project_code": 7,
    }));
    let err = client
        .create(&WorkflowSpec {
            name: "etl".into(),
            definition: json!({"taskRelationJson": []}),
            params: Value::Null,
            schedule: None,
        })
        .await
        .expect_err("缺 taskDefinitionJson");
    assert!(matches!(err, WorkflowError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn config_token_wins_over_headers_token() {
    let app = Router::new().nest(
        API,
        Router::new().route(
            format!("/projects/{PROJECT}/process-instances/{{id}}").as_str(),
            get(ds_token),
        ),
    );
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": base(&url),
        "project_code": 1234567890123_i64,
        "token": "field-value",
        "headers": {"token": "header-value", "X-Extra": "kept"},
        "api_style": "legacy",
    }));

    let status = client
        .status(&RunId("9".into()))
        .await
        .expect("配置字段的 token 必须覆盖 headers 里的 token");
    assert_eq!(status.state, RunState::Running);
}

#[test]
fn config_flattens_http_fields() {
    let cfg: DolphinSchedulerConfig = serde_json::from_value(json!({
        "base_url": "http://ds:12345/dolphinscheduler",
        "project_code": 7,
        "token": "t",
        "tenant_code": "tenant",
        "api_style": "legacy",
        "username": "u",
        "query_timeout_secs": 5,
        "max_concurrency": 2,
    }))
    .unwrap();

    assert_eq!(cfg.http.base_url, "http://ds:12345/dolphinscheduler");
    assert_eq!(cfg.project_code, 7);
    assert_eq!(cfg.token.as_deref(), Some("t"));
    assert_eq!(cfg.tenant_code.as_deref(), Some("tenant"));
    assert_eq!(cfg.api_style.as_deref(), Some("legacy"));
    assert_eq!(cfg.http.username.as_deref(), Some("u"));
    assert_eq!(cfg.http.query_timeout_secs, Some(5));
    assert_eq!(cfg.http.max_concurrency, Some(2));
}

#[test]
fn non_positive_project_code_is_config_error() {
    let cfg: DolphinSchedulerConfig = serde_json::from_value(json!({
        "base_url": "http://127.0.0.1:1",
        "project_code": 0,
    }))
    .unwrap();
    let err = DolphinSchedulerClient::from_config(cfg)
        .err()
        .expect("project_code 必须为正");
    assert!(matches!(err, WorkflowError::Config(_)), "got {err:?}");
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! DolphinScheduler 3.3+ workflow 命名风格的 axum mock 测试：create + release、
//! start 直接返回实例 id、状态/停止/列表路径与字段。不依赖真实 DS。

use axum::Json;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
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

// ── mock handlers ──────────────────────────────────────────

async fn wf_create(Form(form): Form<HashMap<String, String>>) -> Response {
    let expected = [
        ("name", "etl"),
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
    if form.contains_key("tenantCode") {
        return (
            StatusCode::BAD_REQUEST,
            format!("workflow create must not send tenantCode: {form:?}"),
        )
            .into_response();
    }
    (
        StatusCode::CREATED,
        Json(json!({"code": 0, "msg": "success", "data": {
            "code": 42,
            "name": "etl",
        }})),
    )
        .into_response()
}

async fn wf_release(
    State(releases): State<Arc<AtomicUsize>>,
    Path(code): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    if code != "42" || form.get("releaseState").map(String::as_str) != Some("ONLINE") {
        return (
            StatusCode::BAD_REQUEST,
            format!("bad release: {code} {form:?}"),
        )
            .into_response();
    }
    releases.fetch_add(1, Ordering::SeqCst);
    Json(json!({"code": 0, "msg": "success", "data": true})).into_response()
}

async fn wf_start(Form(form): Form<HashMap<String, String>>) -> Response {
    let ok = form.get("workflowDefinitionCode").map(String::as_str) == Some("42")
        && form.get("failureStrategy").map(String::as_str) == Some("CONTINUE")
        && form.get("warningType").map(String::as_str) == Some("NONE")
        && form.get("workflowInstancePriority").map(String::as_str) == Some("MEDIUM")
        && form.get("workerGroup").map(String::as_str) == Some("default")
        && form.get("startParams").map(String::as_str) == Some(r#"{"k":"v"}"#)
        && form.get("scheduleTime").is_some_and(|t| t.len() == 19);
    if !ok {
        return (StatusCode::BAD_REQUEST, format!("bad form: {form:?}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": [777]})).into_response()
}

async fn wf_start_trigger_code(Form(form): Form<HashMap<String, String>>) -> Response {
    if form.get("workflowDefinitionCode").map(String::as_str) != Some("42") {
        return (StatusCode::BAD_REQUEST, format!("bad form: {form:?}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": 555})).into_response()
}

async fn wf_poll(
    State(polls): State<Arc<AtomicUsize>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if query.get("triggerCode").map(String::as_str) != Some("555") {
        return (StatusCode::BAD_REQUEST, format!("bad query: {query:?}")).into_response();
    }
    let attempt = polls.fetch_add(1, Ordering::SeqCst);
    let data = if attempt == 0 {
        json!([])
    } else {
        json!([{"id": 888}])
    };
    Json(json!({"code": 0, "msg": "success", "data": data})).into_response()
}

async fn wf_status(Path(id): Path<String>) -> Response {
    if id != "777" {
        return (StatusCode::BAD_REQUEST, format!("bad instance id: {id}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": {
        "id": 777,
        "workflowDefinitionCode": 42,
        "state": "SUCCESS",
        "startTime": "2026-10-29 10:00:00",
        "endTime": "2026-10-29 10:05:00",
    }}))
    .into_response()
}

async fn wf_cancel(Query(query): Query<HashMap<String, String>>) -> Response {
    if query.get("workflowInstanceId").map(String::as_str) != Some("777")
        || query.get("executeType").map(String::as_str) != Some("STOP")
    {
        return (StatusCode::BAD_REQUEST, format!("bad query: {query:?}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": null})).into_response()
}

async fn wf_list(Query(query): Query<HashMap<String, String>>) -> Response {
    let ok = query.get("pageNo").map(String::as_str) == Some("1")
        && query.get("pageSize").map(String::as_str) == Some("3")
        && query.get("workflowDefinitionCode").map(String::as_str) == Some("42")
        && query.get("stateType").map(String::as_str) == Some("FAILURE");
    if !ok {
        return (StatusCode::BAD_REQUEST, format!("bad query: {query:?}")).into_response();
    }
    Json(json!({"code": 0, "msg": "success", "data": {
        "totalList": [{
            "id": 778,
            "workflowDefinitionCode": 42,
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

// ── 用例 ───────────────────────────────────────────────────

#[tokio::test]
async fn workflow_style_create_release_trigger_status_cancel_list() {
    let releases = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .nest(
            API,
            Router::new()
                .route(
                    format!("/projects/{PROJECT}/workflow-definition").as_str(),
                    post(wf_create),
                )
                .route(
                    format!("/projects/{PROJECT}/workflow-definition/{{code}}/release").as_str(),
                    post(wf_release),
                )
                .route(
                    format!("/projects/{PROJECT}/executors/start-workflow-instance").as_str(),
                    post(wf_start),
                )
                .route(
                    format!("/projects/{PROJECT}/workflow-instances/{{id}}").as_str(),
                    get(wf_status),
                )
                .route(
                    format!("/projects/{PROJECT}/executors/execute").as_str(),
                    post(wf_cancel),
                )
                .route(
                    format!("/projects/{PROJECT}/workflow-instances").as_str(),
                    get(wf_list),
                ),
        )
        .with_state(Arc::clone(&releases));
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": format!("{url}{API}"),
        "project_code": 1234567890123_i64,
    }));

    assert_eq!(client.engine(), "dolphinscheduler");
    let caps = client.capabilities();
    assert!(caps.create && caps.trigger && caps.cancel && caps.list_runs);

    let id = client
        .create(&WorkflowSpec {
            name: "etl".into(),
            definition: json!({
                "taskDefinitionJson": [{"code": 1}],
                "taskRelationJson": r#"[{"x":1}]"#,
                "locations": [{"taskCode": 1, "x": 0, "y": 0}],
            }),
            params: Value::Null,
            schedule: None,
        })
        .await
        .expect("create");
    assert_eq!(id, WorkflowId("42".into()));
    assert_eq!(
        releases.load(Ordering::SeqCst),
        1,
        "workflow 风格 create 必须紧接着 release ONLINE"
    );

    let run = client
        .trigger(&id, json!({"k": "v"}))
        .await
        .expect("trigger");
    assert_eq!(run, RunId("777".into()));

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
async fn workflow_start_with_trigger_code_is_polled() {
    let polls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .nest(
            API,
            Router::new()
                .route(
                    format!("/projects/{PROJECT}/executors/start-workflow-instance").as_str(),
                    post(wf_start_trigger_code),
                )
                .route(
                    format!("/projects/{PROJECT}/workflow-instances/trigger").as_str(),
                    get(wf_poll),
                ),
        )
        .with_state(Arc::clone(&polls));
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": format!("{url}{API}"),
        "project_code": 1234567890123_i64,
    }));

    let run = client
        .trigger(&WorkflowId("42".into()), Value::Null)
        .await
        .expect("trigger");
    assert_eq!(run, RunId("888".into()));
    assert_eq!(
        polls.load(Ordering::SeqCst),
        2,
        "第一次轮询空列表后必须继续直到实例出现"
    );
}

#[test]
fn api_style_defaults_to_workflow_and_rejects_unknown() {
    let cfg: DolphinSchedulerConfig = serde_json::from_value(json!({
        "base_url": "http://ds:12345/dolphinscheduler",
        "project_code": 7,
    }))
    .unwrap();
    assert_eq!(cfg.api_style, None);

    let cfg: DolphinSchedulerConfig = serde_json::from_value(json!({
        "base_url": "http://ds:12345/dolphinscheduler",
        "project_code": 7,
        "api_style": "v3",
    }))
    .unwrap();
    let err = DolphinSchedulerClient::from_config(cfg)
        .err()
        .expect("未知 api_style 必须被拒");
    assert!(matches!(err, WorkflowError::Config(_)), "got {err:?}");
}

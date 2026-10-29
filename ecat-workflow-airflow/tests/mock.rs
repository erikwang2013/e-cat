// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Airflow 适配器对 axum mock 的行为测试：v2/v1 路径与请求体、状态映射、错误
//! 映射、配置 flatten、超时与熔断。不依赖真实 Airflow。

use axum::Router;
use axum::extract::{Json, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use ecat_workflow::{
    RunFilter, RunId, RunState, WorkflowClient, WorkflowError, WorkflowId, WorkflowSpec,
};
use ecat_workflow_airflow::{AirflowClient, AirflowConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn client_from(config: Value) -> AirflowClient {
    AirflowClient::from_config(serde_json::from_value(config).unwrap()).unwrap()
}

// ── mock handlers ──────────────────────────────────────────

async fn airflow_trigger(Json(body): Json<Value>) -> Response {
    if body["conf"] != json!({"k": "v"}) || !body.get("logical_date").is_some_and(Value::is_null) {
        return (StatusCode::BAD_REQUEST, format!("bad body: {body}")).into_response();
    }
    let Some(run_id) = body["dag_run_id"].as_str() else {
        return (StatusCode::BAD_REQUEST, "expected a generated dag_run_id").into_response();
    };
    Json(json!({"dag_id": "etl", "dag_run_id": run_id, "state": "queued"})).into_response()
}

async fn airflow_trigger_explicit(Json(body): Json<Value>) -> Response {
    if body["logical_date"] != json!("2026-10-29T00:00:00Z") || body["conf"]["k"] != json!("v") {
        return (StatusCode::BAD_REQUEST, format!("bad body: {body}")).into_response();
    }
    Json(json!({"dag_id": "explicit", "dag_run_id": "explicit-1", "state": "queued"}))
        .into_response()
}

async fn airflow_trigger_v1(Json(body): Json<Value>) -> Response {
    if body["conf"] != json!({"k": "v"}) || body.get("logical_date").is_some() {
        return (StatusCode::BAD_REQUEST, format!("bad v1 body: {body}")).into_response();
    }
    Json(json!({"dag_id": "etl", "dag_run_id": "v1-run", "state": "queued"})).into_response()
}

async fn airflow_status(Path(run_id): Path<String>) -> Response {
    if !run_id.starts_with("manual-") {
        return (StatusCode::BAD_REQUEST, format!("bad run id: {run_id}")).into_response();
    }
    Json(json!({
        "dag_id": "etl",
        "dag_run_id": run_id,
        "state": "success",
        "start_date": "2026-10-29T10:00:00Z",
        "end_date": "2026-10-29T10:05:00Z",
    }))
    .into_response()
}

async fn airflow_cancel(Path(run_id): Path<String>, Json(body): Json<Value>) -> Response {
    if body != json!({"state": "failed"}) {
        return (StatusCode::BAD_REQUEST, format!("bad body: {body}")).into_response();
    }
    Json(json!({"dag_id": "etl", "dag_run_id": run_id, "state": "failed"})).into_response()
}

async fn airflow_list(Query(query): Query<HashMap<String, String>>) -> Response {
    if query.get("limit").map(String::as_str) != Some("5")
        || query.get("state").map(String::as_str) != Some("failed")
    {
        return (StatusCode::BAD_REQUEST, format!("bad query: {query:?}")).into_response();
    }
    Json(json!({
        "dag_runs": [{
            "dag_id": "etl",
            "dag_run_id": "manual-1",
            "state": "failed",
            "start_date": "2026-10-29T10:00:00Z",
            "end_date": null,
        }],
        "total_entries": 1,
    }))
    .into_response()
}

async fn airflow_trigger_null(Json(body): Json<Value>) -> Response {
    if body["conf"] != json!({})
        || body.get("dag_run_id").is_some()
        || !body.get("logical_date").is_some_and(Value::is_null)
    {
        return (StatusCode::BAD_REQUEST, format!("bad body: {body}")).into_response();
    }
    Json(json!({"dag_id": "etl", "dag_run_id": "airflow-generated", "state": "queued"}))
        .into_response()
}

async fn boom() -> (StatusCode, &'static str) {
    (StatusCode::INTERNAL_SERVER_ERROR, "boom")
}

async fn not_json() -> &'static str {
    "not json"
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "{}"
}

// ── 用例 ───────────────────────────────────────────────────

#[tokio::test]
async fn trigger_status_cancel_and_list_use_v2_paths() {
    let app = Router::new()
        .route(
            "/api/v2/dags/etl/dagRuns",
            post(airflow_trigger).get(airflow_list),
        )
        .route(
            "/api/v2/dags/etl/dagRuns/{run_id}",
            get(airflow_status).patch(airflow_cancel),
        );
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": url,
        "dag_run_id_prefix": "manual-",
    }));

    assert_eq!(client.engine(), "airflow");
    let caps = client.capabilities();
    assert!(caps.trigger && caps.cancel && caps.list_runs && !caps.create);

    let run = client
        .trigger(&WorkflowId("etl".into()), json!({"k": "v"}))
        .await
        .expect("trigger");
    assert!(run.0.starts_with("etl/manual-"), "got {run:?}");

    let status = client.status(&run).await.expect("status");
    assert_eq!(status.state, RunState::Succeeded);
    assert_eq!(status.workflow_id, WorkflowId("etl".into()));
    assert_eq!(status.run_id, run);
    assert_eq!(status.started_at.as_deref(), Some("2026-10-29T10:00:00Z"));
    assert_eq!(status.finished_at.as_deref(), Some("2026-10-29T10:05:00Z"));

    client.cancel(&run).await.expect("cancel");

    let runs = client
        .list_runs(&RunFilter {
            workflow_id: Some(WorkflowId("etl".into())),
            state: Some(RunState::Failed),
            limit: Some(5),
        })
        .await
        .expect("list");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Failed);
    assert_eq!(runs[0].run_id, RunId("etl/manual-1".into()));
}

#[tokio::test]
async fn v2_null_params_send_null_logical_date_and_empty_conf() {
    let app = Router::new().route("/api/v2/dags/etl/dagRuns", post(airflow_trigger_null));
    let url = serve(app).await;
    let client = client_from(json!({"base_url": url}));

    let run = client
        .trigger(&WorkflowId("etl".into()), Value::Null)
        .await
        .expect("trigger");
    assert_eq!(run, RunId("etl/airflow-generated".into()));
}

#[tokio::test]
async fn base_url_with_api_suffix_is_not_doubled() {
    let app = Router::new().route("/api/v2/dags/etl/dagRuns", post(airflow_trigger_null));
    let url = serve(app).await;
    // 用户若把 API 根写进 base_url，必须归一化，不能拼成 /api/v1/api/v2/...
    for suffix in ["/api/v1", "/api/v2"] {
        let client = client_from(json!({"base_url": format!("{url}{suffix}")}));
        let run = client
            .trigger(&WorkflowId("etl".into()), Value::Null)
            .await
            .expect("trigger");
        assert_eq!(run, RunId("etl/airflow-generated".into()));
    }
}

#[tokio::test]
async fn v2_trigger_reuses_explicit_logical_date_from_params() {
    let app = Router::new().route(
        "/api/v2/dags/explicit/dagRuns",
        post(airflow_trigger_explicit),
    );
    let url = serve(app).await;
    let client = client_from(json!({"base_url": url}));

    let run = client
        .trigger(
            &WorkflowId("explicit".into()),
            json!({"logical_date": "2026-10-29T00:00:00Z", "k": "v"}),
        )
        .await
        .expect("trigger");
    assert_eq!(run, RunId("explicit/explicit-1".into()));
}

#[tokio::test]
async fn v1_trigger_omits_logical_date() {
    let app = Router::new().route("/api/v1/dags/etl/dagRuns", post(airflow_trigger_v1));
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": url,
        "api_version": "v1",
    }));

    let run = client
        .trigger(&WorkflowId("etl".into()), json!({"k": "v"}))
        .await
        .expect("trigger");
    assert_eq!(run, RunId("etl/v1-run".into()));
}

#[tokio::test]
async fn list_without_dag_id_is_remote_error() {
    let client = client_from(json!({"base_url": "http://127.0.0.1:1"}));
    let err = client
        .list_runs(&RunFilter::default())
        .await
        .expect_err("Airflow 列表必须带 dag_id");
    assert!(
        matches!(err, WorkflowError::Remote(ref msg) if msg.contains("workflow_id (dag_id)")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn create_is_unsupported() {
    let client = client_from(json!({"base_url": "http://127.0.0.1:1"}));
    let err = client
        .create(&WorkflowSpec {
            name: "etl".into(),
            definition: Value::Null,
            params: Value::Null,
            schedule: None,
        })
        .await
        .expect_err("Airflow REST 不能创建 DAG");
    assert!(matches!(err, WorkflowError::Unsupported(_)), "got {err:?}");
}

#[test]
fn config_flattens_http_fields() {
    let cfg: AirflowConfig = serde_json::from_value(json!({
        "base_url": "http://airflow:8080",
        "api_version": "v1",
        "username": "u",
        "password": "p",
        "headers": {"X-Api-Key": "k"},
        "query_timeout_secs": 5,
        "max_concurrency": 4,
        "breaker": {"failure_ratio": 0.9, "window": 60},
        "dag_run_id_prefix": "manual-",
    }))
    .unwrap();

    assert_eq!(cfg.http.base_url, "http://airflow:8080");
    assert_eq!(cfg.api_version.as_deref(), Some("v1"));
    assert_eq!(cfg.http.username.as_deref(), Some("u"));
    assert_eq!(
        cfg.http.headers.get("X-Api-Key").map(String::as_str),
        Some("k")
    );
    assert_eq!(cfg.http.query_timeout_secs, Some(5));
    assert_eq!(cfg.http.max_concurrency, Some(4));
    assert_eq!(
        cfg.http.breaker.as_ref().unwrap().window,
        Duration::from_secs(60)
    );
    assert_eq!(cfg.dag_run_id_prefix.as_deref(), Some("manual-"));
}

#[test]
fn api_version_defaults_to_v2_and_rejects_unknown() {
    let cfg: AirflowConfig =
        serde_json::from_value(json!({"base_url": "http://airflow:8080"})).unwrap();
    assert_eq!(cfg.api_version, None);

    let cfg: AirflowConfig = serde_json::from_value(json!({
        "base_url": "http://airflow:8080",
        "api_version": "v3",
    }))
    .unwrap();
    let err = AirflowClient::from_config(cfg)
        .err()
        .expect("未知 api_version 必须被拒");
    assert!(matches!(err, WorkflowError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn non_2xx_maps_to_http_error() {
    let app = Router::new().route("/api/v2/dags/etl/dagRuns/r1", get(boom));
    let url = serve(app).await;
    let client = client_from(json!({"base_url": url}));

    let err = client
        .status(&RunId("etl/r1".into()))
        .await
        .expect_err("mock 恒 500");
    match err {
        WorkflowError::Http { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, "boom");
        }
        other => panic!("got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_json_maps_to_parse_error() {
    let app = Router::new().route("/api/v2/dags/etl/dagRuns/r1", get(not_json));
    let url = serve(app).await;
    let client = client_from(json!({"base_url": url}));

    let err = client
        .status(&RunId("etl/r1".into()))
        .await
        .expect_err("非 JSON 响应");
    assert!(matches!(err, WorkflowError::Parse(_)), "got {err:?}");
}

#[tokio::test]
async fn timeout_maps_to_timeout() {
    let app = Router::new().route("/api/v2/dags/etl/dagRuns/slow", get(slow));
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": url,
        "query_timeout_secs": 1,
    }));

    // 外层保险丝：漏包超时的实现会「成功返回」而不是挂死。
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        client.status(&RunId("etl/slow".into())),
    )
    .await
    .expect("guarded 没有开火")
    .expect_err("1 秒超时对 5 秒后端必须失败");
    assert!(matches!(err, WorkflowError::Timeout(_)), "got {err:?}");
}

#[tokio::test]
async fn repeated_failures_open_the_breaker() {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/api/v2/dags/etl/dagRuns/r1",
        get({
            let hits = Arc::clone(&hits);
            move || {
                let hits = Arc::clone(&hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::INTERNAL_SERVER_ERROR, "down")
                }
            }
        }),
    );
    let url = serve(app).await;
    let client = client_from(json!({"base_url": url}));

    for _ in 0..5 {
        let err = client
            .status(&RunId("etl/r1".into()))
            .await
            .expect_err("mock 恒 500");
        assert!(
            matches!(err, WorkflowError::Http { status: 500, .. }),
            "got {err:?}"
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 5);

    let err = client
        .status(&RunId("etl/r1".into()))
        .await
        .expect_err("连续 5 次失败后熔断器必须打开");
    assert!(matches!(err, WorkflowError::Unavailable(_)), "got {err:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 5, "熔断打开后不得再打后端");
}

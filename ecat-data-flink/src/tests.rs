// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! SQL Gateway 协议层测试（axum mock，不依赖真实 Flink）。
use super::*;
use axum::{
    Json, Router,
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};

mod resilience;

/// 一页结果。行形状照 `ResultInfoSerializer`：`{"kind","fields"}`。
fn page(v: i64) -> serde_json::Value {
    json!({
        "resultType": "PAYLOAD",
        "isQueryResult": true,
        "resultKind": "SUCCESS",
        "results": {
            "columns": [
                {"name": "id", "logicalType": {"type": "BIGINT", "nullable": true}, "comment": null}
            ],
            "rowFormat": "JSON",
            "data": [{"kind": "INSERT", "fields": [v]}]
        }
    })
}

/// 可配置的 SQL Gateway mock。默认是 happy path：
/// session → statement → FINISHED → 一页一行；`pages` 非空时按 token 分页。
#[derive(Default)]
struct Mock {
    sessions: AtomicUsize,
    statements: AtomicUsize,
    status_polls: AtomicUsize,
    result_fetches: AtomicUsize,
    open_bodies: StdMutex<Vec<serde_json::Value>>,
    /// statements 对这个 session 回 404（模拟 session 失效）。
    expired_session: StdMutex<Option<String>>,
    deny_statements: AtomicBool,
    /// 依次弹出的状态；弹空后恒为 FINISHED。
    statuses: StdMutex<Vec<String>>,
    /// 恒为 RUNNING（验证外层超时覆盖轮询阶段）。
    always_running: AtomicBool,
    pages: StdMutex<Vec<serde_json::Value>>,
    submit_status: AtomicU16,
    saw_row_format: AtomicBool,
    open_delay_ms: AtomicU64,
    open_delay_once_ms: AtomicU64,
    statement_hold_ms: AtomicU64,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    /// 第 `barrier_skip` 次之后的 statement 提交都在这里等齐。
    barrier: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
    barrier_skip: AtomicUsize,
}

async fn open_session(
    State(mock): State<Arc<Mock>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let once = mock.open_delay_once_ms.swap(0, Ordering::SeqCst);
    let delay = once.max(mock.open_delay_ms.load(Ordering::SeqCst));
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let n = mock.sessions.fetch_add(1, Ordering::SeqCst) + 1;
    mock.open_bodies.lock().unwrap().push(body);
    Json(json!({"sessionHandle": format!("s{n}")}))
}

async fn statements(State(mock): State<Arc<Mock>>, Path(session): Path<String>) -> Response {
    let n = mock.statements.fetch_add(1, Ordering::SeqCst);
    let barrier = mock.barrier.lock().unwrap().clone();
    if let Some(barrier) = barrier
        && n >= mock.barrier_skip.load(Ordering::SeqCst)
    {
        barrier.wait().await;
    }
    let status = mock.submit_status.load(Ordering::SeqCst);
    if status != 0 {
        return (
            axum::http::StatusCode::from_u16(status).unwrap(),
            "gateway down".to_string(),
        )
            .into_response();
    }
    if mock.deny_statements.load(Ordering::SeqCst)
        || mock.expired_session.lock().unwrap().as_deref() == Some(session.as_str())
    {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    let hold = mock.statement_hold_ms.load(Ordering::SeqCst);
    if hold > 0 {
        let current = mock.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        mock.peak_in_flight.fetch_max(current, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(hold)).await;
        mock.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
    Json(json!({"operationHandle": format!("op{n}")})).into_response()
}

async fn operation_status(State(mock): State<Arc<Mock>>) -> Json<serde_json::Value> {
    mock.status_polls.fetch_add(1, Ordering::SeqCst);
    let status = if mock.always_running.load(Ordering::SeqCst) {
        "RUNNING".to_string()
    } else {
        let mut queue = mock.statuses.lock().unwrap();
        if queue.is_empty() {
            "FINISHED".to_string()
        } else {
            queue.remove(0)
        }
    };
    let mut body = json!({"status": status});
    if status == "ERROR" {
        body["error"] = json!("boom");
    }
    Json(body)
}

async fn operation_result(
    State(mock): State<Arc<Mock>>,
    Path((session, operation, token)): Path<(String, String, u64)>,
    uri: axum::http::Uri,
) -> Json<serde_json::Value> {
    mock.result_fetches.fetch_add(1, Ordering::SeqCst);
    if uri
        .query()
        .is_some_and(|q| q.split('&').any(|p| p == "rowFormat=JSON"))
    {
        mock.saw_row_format.store(true, Ordering::SeqCst);
    }
    let pages = mock.pages.lock().unwrap();
    let mut body = if pages.is_empty() {
        page(1)
    } else {
        pages
            .get(token as usize)
            .cloned()
            .unwrap_or_else(|| json!({"resultType": "EOS"}))
    };
    if (token as usize) + 1 < pages.len() {
        body["nextResultUri"] = json!(format!(
            "/v1/sessions/{session}/operations/{operation}/result/{}",
            token + 1
        ));
    }
    Json(body)
}

async fn spawn(mock: Arc<Mock>) -> String {
    let app = Router::new()
        .route("/v1/sessions", post(open_session))
        .route("/v1/sessions/{session}/statements", post(statements))
        .route(
            "/v1/sessions/{session}/operations/{operation}/status",
            get(operation_status),
        )
        .route(
            "/v1/sessions/{session}/operations/{operation}/result/{token}",
            get(operation_result),
        )
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn client(url: &str) -> FlinkClient {
    let cfg: FlinkConfig = serde_json::from_str(&format!(r#"{{"base_url":"{url}"}}"#)).unwrap();
    FlinkClient::from_config(cfg).unwrap()
}

#[test]
fn client_constructs() {
    let client = FlinkClient::new("http://localhost:8083/");
    assert!(!client.base_url.ends_with('/'), "base_url 应去掉尾部斜杠");
}

#[test]
fn config_with_optional_auth_and_properties() {
    let cfg: FlinkConfig = serde_json::from_str(
        r#"{"base_url":"http://localhost:8083","username":"admin","password":"flink",
            "session_properties":{"execution.runtime-mode":"batch"}}"#,
    )
    .unwrap();
    let client = FlinkClient::from_config(cfg).unwrap();
    assert!(client.username.is_some());
    assert_eq!(
        client
            .session_properties
            .get("execution.runtime-mode")
            .map(String::as_str),
        Some("batch")
    );
}

#[tokio::test]
async fn session_is_created_lazily_and_reused() {
    let mock = Arc::new(Mock::default());
    let url = spawn(Arc::clone(&mock)).await;
    let client = client(&url);
    assert_eq!(
        mock.sessions.load(Ordering::SeqCst),
        0,
        "构造器不得做 I/O（session 必须懒创建）"
    );

    let rows = client.query("select 1").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&json!(1)));

    client.query("select 2").await.unwrap();
    assert_eq!(mock.sessions.load(Ordering::SeqCst), 1, "session 必须复用");
    assert_eq!(mock.statements.load(Ordering::SeqCst), 2);
    assert_eq!(mock.result_fetches.load(Ordering::SeqCst), 2);
    assert!(
        mock.saw_row_format.load(Ordering::SeqCst),
        "拉结果必须带 rowFormat=JSON"
    );
}

#[tokio::test]
async fn session_properties_are_sent_verbatim() {
    let mock = Arc::new(Mock::default());
    let url = spawn(Arc::clone(&mock)).await;
    let cfg: FlinkConfig = serde_json::from_str(&format!(
        r#"{{"base_url":"{url}","session_properties":{{"execution.runtime-mode":"batch"}}}}"#
    ))
    .unwrap();
    FlinkClient::from_config(cfg)
        .unwrap()
        .query("select 1")
        .await
        .unwrap();

    let bodies = mock.open_bodies.lock().unwrap();
    assert_eq!(
        bodies[0],
        json!({"properties": {"execution.runtime-mode": "batch"}})
    );
}

#[tokio::test]
async fn query_polls_until_finished() {
    let mock = Arc::new(Mock::default());
    mock.statuses.lock().unwrap().push("RUNNING".to_string());
    let url = spawn(Arc::clone(&mock)).await;
    client(&url).query("select 1").await.unwrap();
    assert_eq!(mock.status_polls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn query_aggregates_paginated_results() {
    let mock = Arc::new(Mock::default());
    *mock.pages.lock().unwrap() = vec![page(1), page(2)];
    let url = spawn(Arc::clone(&mock)).await;
    let rows = client(&url).query("select * from t").await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("id"), Some(&json!(1)));
    assert_eq!(rows[1].get("id"), Some(&json!(2)));
    assert_eq!(
        mock.result_fetches.load(Ordering::SeqCst),
        2,
        "必须跟随 nextResultUri 拉完两页"
    );
}

#[tokio::test]
async fn execute_returns_zero_and_skips_result_fetch() {
    let mock = Arc::new(Mock::default());
    let url = spawn(Arc::clone(&mock)).await;
    let client = client(&url);
    assert_eq!(
        client.execute("create table t (i bigint)").await.unwrap(),
        0
    );
    assert_eq!(mock.statements.load(Ordering::SeqCst), 1);
    assert_eq!(
        mock.result_fetches.load(Ordering::SeqCst),
        0,
        "execute 不应拉结果集"
    );
}

#[tokio::test]
async fn error_status_is_reported() {
    let mock = Arc::new(Mock::default());
    mock.statuses.lock().unwrap().push("ERROR".to_string());
    let url = spawn(Arc::clone(&mock)).await;
    let err = client(&url).query("select broken").await.unwrap_err();
    assert!(err.to_string().contains("boom"), "got: {err}");
}

#[tokio::test]
async fn http_error_body_is_reported() {
    let mock = Arc::new(Mock::default());
    mock.submit_status.store(500, Ordering::SeqCst);
    let url = spawn(Arc::clone(&mock)).await;
    let err = client(&url).query("select 1").await.unwrap_err();
    assert!(err.to_string().contains("gateway down"), "got: {err}");
}

#[tokio::test]
async fn expired_session_is_rebuilt_and_reused() {
    let mock = Arc::new(Mock::default());
    let url = spawn(Arc::clone(&mock)).await;
    let client = client(&url);

    client.query("select 1").await.unwrap();
    *mock.expired_session.lock().unwrap() = Some("s1".to_string());

    client.query("select 2").await.unwrap();
    assert_eq!(mock.sessions.load(Ordering::SeqCst), 2, "404 后应重建一次");

    client.query("select 3").await.unwrap();
    assert_eq!(
        mock.sessions.load(Ordering::SeqCst),
        2,
        "重建后的 session 必须复用"
    );
    assert_eq!(mock.statements.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn expired_session_is_rebuilt_only_once() {
    let mock = Arc::new(Mock::default());
    mock.deny_statements.store(true, Ordering::SeqCst);
    let url = spawn(Arc::clone(&mock)).await;
    let err = client(&url).query("select 1").await.unwrap_err();
    assert!(err.to_string().contains("session not found"), "got: {err}");
    assert_eq!(
        mock.sessions.load(Ordering::SeqCst),
        2,
        "只允许重建一次，不能无限重试"
    );
}

#[tokio::test]
async fn transaction_returns_not_supported_error() {
    let client = FlinkClient::new("http://localhost:8083");
    let err = match client.transaction().await {
        Err(e) => e,
        Ok(_) => panic!("expected unsupported error"),
    };
    assert!(
        err.to_string().contains("does not support transactions"),
        "got: {err}"
    );
}

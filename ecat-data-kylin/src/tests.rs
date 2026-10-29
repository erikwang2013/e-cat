// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 单元测试。独立成文件：`lib.rs` 加出站韧性后会顶到 500 行硬上限。

mod resilience;

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Captured {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

/// mock Kylin 端点（fallback 捕获任意路径）：捕获路径/头/体，按给定状态码与响应体应答。
async fn spawn_mock(
    captured: Arc<Mutex<Vec<Captured>>>,
    status: u16,
    body: &'static str,
) -> String {
    let app = axum::Router::new().fallback(move |req: axum::http::Request<Body>| async move {
        let (parts, req_body) = req.into_parts();
        let headers = parts
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let req_body = to_bytes(req_body, usize::MAX).await.unwrap_or_default();
        captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Captured {
                path: parts.uri.path().to_string(),
                headers,
                body: String::from_utf8_lossy(&req_body).into_owned(),
            });
        let mut resp = Response::new(Body::from(body));
        *resp.status_mut() = StatusCode::from_u16(status).unwrap();
        resp
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[test]
fn config_deserializes_project_and_auth() {
    let cfg: KylinConfig = serde_json::from_str(
        r#"{"base_url":"http://localhost:7070","username":"ADMIN",
            "password":"KYLIN","project":"learn_kylin"}"#,
    )
    .unwrap();
    assert_eq!(cfg.project.as_deref(), Some("learn_kylin"));
    assert!(cfg.tls.is_none());
}

#[test]
fn client_constructs() {
    let _client = KylinClient::new("http://localhost:7070");
}

#[test]
fn dialect_is_standard() {
    let client = KylinClient::new("http://localhost:7070");
    assert_eq!(client.dialect(), Dialect::Standard);
}

#[tokio::test]
async fn query_posts_expected_body_and_parses_rows() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let body = r#"{
        "columnMetas": [{"name": "ID"}, {"name": "NAME"}],
        "results": [["1"], "oops", ["3", "carol", "extra"]],
        "isException": false,
        "exceptionMessage": null
    }"#;
    let base_url = spawn_mock(captured.clone(), 200, body).await;
    let client = KylinClient::new(base_url);
    let rows = client.query("select id, name from t").await.unwrap();

    assert_eq!(rows.len(), 2, "非数组的畸形行应跳过");
    assert_eq!(
        rows[0]
            .columns()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["ID", "NAME"]
    );
    assert_eq!(rows[0].get("ID"), Some(&serde_json::json!("1")));
    assert_eq!(
        rows[0].get("NAME"),
        Some(&serde_json::Value::Null),
        "短行缺的列补 null"
    );
    assert_eq!(rows[1].get("NAME"), Some(&serde_json::json!("carol")));

    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs[0].path, "/kylin/api/query");
    assert_eq!(reqs[0].json()["sql"], "select id, name from t");
    assert_eq!(reqs[0].json()["offset"], serde_json::json!(0));
    assert_eq!(reqs[0].json()["limit"], serde_json::json!(DEFAULT_LIMIT));
    assert!(
        reqs[0].json().get("project").is_none(),
        "未配置 project 时不应发送该字段"
    );
}

#[tokio::test]
async fn from_config_sends_project_and_basic_auth() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let body = r#"{"columnMetas": [], "results": [], "isException": false}"#;
    let base_url = spawn_mock(captured.clone(), 200, body).await;
    let cfg: KylinConfig = serde_json::from_str(&format!(
        r#"{{"base_url":"{base_url}","username":"u","password":"p",
            "project":"learn_kylin"}}"#
    ))
    .unwrap();
    let client = KylinClient::from_config(cfg).unwrap();
    client.query("select 1").await.unwrap();

    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs[0].json()["project"], "learn_kylin");
    assert_eq!(reqs[0].header("authorization"), Some("Basic dTpw"));
}

#[tokio::test]
async fn query_unwraps_v4_public_envelope() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let body = r#"{
        "code": "000",
        "data": {
            "columnMetas": [{"name": "X"}],
            "results": [[7]],
            "isException": false
        },
        "msg": ""
    }"#;
    let base_url = spawn_mock(captured, 200, body).await;
    let client = KylinClient::new(base_url);
    let rows = client.query("select 7 as x").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("X"), Some(&serde_json::json!(7)));
}

#[tokio::test]
async fn query_reports_exception_message() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let body = r#"{
        "columnMetas": [],
        "results": [],
        "isException": true,
        "exceptionMessage": "No realization found for the query"
    }"#;
    let base_url = spawn_mock(captured, 200, body).await;
    let client = KylinClient::new(base_url);
    let err = client.query("select bad").await.unwrap_err();
    assert!(
        err.to_string().contains("No realization found"),
        "got: {err}"
    );
}

#[tokio::test]
async fn query_reports_http_error_body() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured, 500, "kylin exploded").await;
    let client = KylinClient::new(base_url);
    let err = client.query("select 1").await.unwrap_err();
    assert!(err.to_string().contains("kylin exploded"), "got: {err}");
}

#[tokio::test]
async fn query_reports_results_without_column_metas() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured, 200, r#"{"results": [[1]], "isException": false}"#).await;
    let client = KylinClient::new(base_url);
    let err = client.query("select 1").await.unwrap_err();
    assert!(
        err.to_string().contains("without columnMetas"),
        "got: {err}"
    );
}

#[tokio::test]
async fn query_reports_v4_envelope_error_code() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let body = r#"{"code":"10001","data":null,"msg":"project not found"}"#;
    let base_url = spawn_mock(captured, 200, body).await;
    let client = KylinClient::new(base_url);
    let err = client.query("select 1").await.unwrap_err();
    assert!(err.to_string().contains("project not found"), "got: {err}");
    assert!(err.to_string().contains("10001"), "got: {err}");
}

#[tokio::test]
async fn execute_returns_zero_on_success() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let body = r#"{"columnMetas": [], "results": [], "isException": false}"#;
    let base_url = spawn_mock(captured.clone(), 200, body).await;
    let client = KylinClient::new(base_url);
    assert_eq!(client.execute("select 1").await.unwrap(), 0);
    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs[0].path, "/kylin/api/query");
}

#[tokio::test]
async fn transaction_returns_not_supported_error() {
    let client = KylinClient::new("http://localhost:7070");
    let err = match client.transaction().await {
        Err(e) => e,
        Ok(_) => panic!("expected unsupported error"),
    };
    assert!(
        err.to_string().contains("does not support transactions"),
        "got: {err}"
    );
}

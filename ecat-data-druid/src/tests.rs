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

/// mock Druid 端点（fallback 捕获任意路径）：捕获路径/头/体，按给定状态码与响应体应答。
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
fn config_deserializes_context_and_auth() {
    let cfg: DruidConfig = serde_json::from_str(
        r#"{"base_url":"http://localhost:8888","username":"u","password":"p",
            "context":{"priority":5,"useCache":true}}"#,
    )
    .unwrap();
    assert_eq!(cfg.context["priority"], serde_json::json!(5));
    assert_eq!(cfg.context["useCache"], serde_json::json!(true));
    assert!(cfg.tls.is_none());
}

#[test]
fn client_constructs() {
    let _client = DruidClient::new("http://localhost:8888");
}

#[test]
fn dialect_is_standard() {
    let client = DruidClient::new("http://localhost:8888");
    assert_eq!(client.dialect(), Dialect::Standard);
}

#[tokio::test]
async fn query_posts_sql_and_parses_rows_in_first_object_key_order() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured.clone(), 200, r#"[{"b":1,"a":2},{"b":3}]"#).await;
    let client = DruidClient::new(base_url);
    let rows = client.query("select b, a from t").await.unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]
            .columns()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["b", "a"],
        "列顺序必须取首个对象的 key 文档顺序，不能按字母排序"
    );
    assert_eq!(rows[0].get("a"), Some(&serde_json::json!(2)));
    assert_eq!(rows[1].get("b"), Some(&serde_json::json!(3)));
    assert_eq!(
        rows[1].get("a"),
        Some(&serde_json::Value::Null),
        "后续行缺失的列必须补 null"
    );

    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs[0].path, "/druid/v2/sql");
    assert_eq!(reqs[0].json()["query"], "select b, a from t");
}

#[tokio::test]
async fn query_returns_empty_rows_for_empty_result_array() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured, 200, "[]").await;
    let client = DruidClient::new(base_url);
    assert!(client.query("select * from t").await.unwrap().is_empty());
}

#[tokio::test]
async fn query_reports_error_message_from_http_error_body() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(
        captured,
        400,
        r#"{"error":"SQL","errorMessage":"table not found","errorClass":"x"}"#,
    )
    .await;
    let client = DruidClient::new(base_url);
    let err = client.query("select * from nope").await.unwrap_err();
    assert!(err.to_string().contains("table not found"), "got: {err}");
}

#[tokio::test]
async fn query_falls_back_to_raw_body_without_error_fields() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured, 500, "backend down").await;
    let client = DruidClient::new(base_url);
    let err = client.query("select 1").await.unwrap_err();
    assert!(err.to_string().contains("backend down"), "got: {err}");
}

#[tokio::test]
async fn query_reports_2xx_error_body() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured, 200, r#"{"errorMessage":"query failed"}"#).await;
    let client = DruidClient::new(base_url);
    let err = client.query("select 1").await.unwrap_err();
    assert!(err.to_string().contains("query failed"), "got: {err}");
}

#[tokio::test]
async fn query_reports_parse_error_for_non_array_body() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured, 200, r#"{"foo":1}"#).await;
    let client = DruidClient::new(base_url);
    let err = client.query("select 1").await.unwrap_err();
    assert!(err.to_string().contains("druid parse"), "got: {err}");
}

#[tokio::test]
async fn execute_returns_zero_on_success() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured.clone(), 200, "[]").await;
    let client = DruidClient::new(base_url);
    assert_eq!(client.execute("select 1").await.unwrap(), 0);
    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs[0].path, "/druid/v2/sql");
}

#[tokio::test]
async fn from_config_merges_context_and_sends_basic_auth() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let base_url = spawn_mock(captured.clone(), 200, "[]").await;
    let cfg: DruidConfig = serde_json::from_str(&format!(
        r#"{{"base_url":"{base_url}","username":"u","password":"p",
            "context":{{"priority":5,"useCache":true}}}}"#
    ))
    .unwrap();
    let client = DruidClient::from_config(cfg).unwrap();
    client.query("select 1").await.unwrap();

    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs[0].json()["query"], "select 1");
    assert_eq!(reqs[0].json()["context"]["priority"], serde_json::json!(5));
    assert_eq!(
        reqs[0].json()["context"]["useCache"],
        serde_json::json!(true)
    );
    assert_eq!(reqs[0].header("authorization"), Some("Basic dTpw"));
}

#[tokio::test]
async fn transaction_returns_not_supported_error() {
    let client = DruidClient::new("http://localhost:8888");
    let err = match client.transaction().await {
        Err(e) => e,
        Ok(_) => panic!("expected unsupported error"),
    };
    assert!(
        err.to_string().contains("does not support transactions"),
        "got: {err}"
    );
}

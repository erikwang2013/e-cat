// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 端点 / 编码 / 认证 / 错误映射测试。全部对着本地 axum mock，不连真实 catalog。
mod resilience;

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("request body must be JSON")
    }
}

type Capture = Arc<Mutex<Vec<Captured>>>;

async fn capture(req: Request<Body>, state: &Capture) {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or_default().to_string();
    let headers = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let body = to_bytes(req.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    state.lock().unwrap().push(Captured {
        method,
        path,
        query,
        headers,
        body,
    });
}

fn serve(app: axum::Router) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 固定状态码 + 固定响应体的 mock。
async fn spawn_mock(status: u16, resp_body: &'static str) -> (String, Capture) {
    let captured: Capture = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::clone(&captured);
    let app = axum::Router::new().fallback(move |req: Request<Body>| {
        let state = Arc::clone(&state);
        async move {
            capture(req, &state).await;
            axum::response::Response::builder()
                .status(StatusCode::from_u16(status).unwrap())
                .body(Body::from(resp_body))
                .unwrap()
        }
    });
    (serve(app), captured)
}

/// 按请求顺序依次返回不同响应体的 mock（分页用）。
async fn spawn_sequence(bodies: Vec<&'static str>) -> (String, Capture) {
    let captured: Capture = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::clone(&captured);
    let idx = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |req: Request<Body>| {
        let state = Arc::clone(&state);
        let idx = Arc::clone(&idx);
        let bodies = bodies.clone();
        async move {
            capture(req, &state).await;
            let body = bodies
                .get(idx.fetch_add(1, Ordering::SeqCst))
                .copied()
                .unwrap_or("{}");
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(body))
                .unwrap()
        }
    });
    (serve(app), captured)
}

fn first(captured: &Capture) -> Captured {
    captured
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("mock received no request")
}

fn client_from(json: &str) -> IcebergClient {
    IcebergClient::from_config(serde_json::from_str(json).unwrap()).unwrap()
}

#[tokio::test]
async fn list_namespaces_gets_v1_namespaces() {
    let (url, captured) = spawn_mock(200, r#"{"namespaces":[["a"],["b","c"]]}"#).await;
    let c = IcebergClient::new(url);
    assert_eq!(
        c.list_namespaces().await.unwrap(),
        vec![vec!["a".to_string()], vec!["b".into(), "c".into()]]
    );
    let req = first(&captured);
    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/v1/namespaces");
}

#[tokio::test]
async fn list_namespaces_follows_next_page_token() {
    let (url, captured) = spawn_sequence(vec![
        r#"{"namespaces":[["a"]],"next-page-token":"tok 1"}"#,
        r#"{"namespaces":[["b"]]}"#,
    ])
    .await;
    let c = IcebergClient::new(url);
    assert_eq!(
        c.list_namespaces().await.unwrap(),
        vec![vec!["a".to_string()], vec!["b".to_string()]]
    );
    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].path, "/v1/namespaces");
    assert_eq!(
        reqs[1].query, "pageToken=tok%201",
        "第二页必须带上游标（分隔符/空格按 query 编码）"
    );
}

#[tokio::test]
async fn create_namespace_posts_namespace_and_properties() {
    let (url, captured) = spawn_mock(
        200,
        r#"{"namespace":["prod"],"properties":{"owner":"data"}}"#,
    )
    .await;
    let c = IcebergClient::new(url);
    c.create_namespace(&["prod".into()], Some(&json!({"owner": "data"})))
        .await
        .unwrap();
    c.create_namespace(&["stage".into()], None).await.unwrap();

    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/v1/namespaces");
    assert_eq!(
        reqs[0].json(),
        json!({"namespace": ["prod"], "properties": {"owner": "data"}})
    );
    assert_eq!(
        reqs[1].json(),
        json!({"namespace": ["stage"], "properties": {}}),
        "省略 properties 时必须发送规范默认的空对象"
    );
}

#[tokio::test]
async fn list_tables_uses_encoded_multi_level_namespace() {
    let (url, captured) = spawn_mock(
        200,
        r#"{"identifiers":[{"namespace":["prod","analytics"],"name":"events"}]}"#,
    )
    .await;
    let c = IcebergClient::new(url);
    assert_eq!(
        c.list_tables(&["prod".into(), "analytics".into()])
            .await
            .unwrap(),
        vec!["events".to_string()]
    );
    assert_eq!(
        first(&captured).path,
        "/v1/namespaces/prod%1Fanalytics/tables",
        "多级 namespace 必须以 %1F 连接"
    );
}

#[tokio::test]
async fn list_tables_follows_next_page_token() {
    let (url, captured) = spawn_sequence(vec![
        r#"{"identifiers":[{"namespace":["ns"],"name":"a"}],"next-page-token":"t"}"#,
        r#"{"identifiers":[{"namespace":["ns"],"name":"b"}]}"#,
    ])
    .await;
    let c = IcebergClient::new(url);
    assert_eq!(
        c.list_tables(&["ns".into()]).await.unwrap(),
        vec!["a".to_string(), "b".to_string()]
    );
    assert_eq!(captured.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn load_table_returns_raw_metadata() {
    let body = r#"{"metadata-location":"s3://b/t/v1.json","metadata":{"format-version":2}}"#;
    let (url, captured) = spawn_mock(200, body).await;
    let c = IcebergClient::new(url);
    let value = c.load_table(&["prod".into()], "events 2026").await.unwrap();
    assert_eq!(value["metadata"]["format-version"], 2);
    assert_eq!(
        first(&captured).path,
        "/v1/namespaces/prod/tables/events%202026",
        "表名必须按路径段编码"
    );
}

#[tokio::test]
async fn create_table_posts_schema_and_optional_fields() {
    let (url, captured) = spawn_mock(
        200,
        r#"{"metadata-location":"s3://b/t/v1.json","metadata":{"format-version":2}}"#,
    )
    .await;
    let c = IcebergClient::new(url);
    let schema = json!({"type": "struct", "schema-id": 0, "fields": []});
    let props = json!({"write.format.default": "parquet"});

    let created = c
        .create_table(
            &["prod".into()],
            "events",
            &schema,
            Some("s3://b/t"),
            Some(&props),
        )
        .await
        .unwrap();
    assert_eq!(created["metadata-location"], "s3://b/t/v1.json");

    c.create_table(&["stage".into()], "raw", &schema, None, None)
        .await
        .unwrap();

    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/v1/namespaces/prod/tables");
    assert_eq!(
        reqs[0].json(),
        json!({
            "name": "events",
            "schema": schema,
            "location": "s3://b/t",
            "properties": props,
        })
    );
    assert_eq!(
        reqs[1].json(),
        json!({"name": "raw", "schema": schema}),
        "None 的可选字段不得出现在 body 里"
    );
}

#[tokio::test]
async fn drop_table_issues_delete() {
    let (url, captured) = spawn_mock(204, "").await;
    let c = IcebergClient::new(url);
    c.drop_table(&["prod".into()], "events").await.unwrap();
    let req = first(&captured);
    assert_eq!(req.method, "DELETE");
    assert_eq!(req.path, "/v1/namespaces/prod/tables/events");
}

#[tokio::test]
async fn config_reads_defaults_and_overrides() {
    let body = r#"{"defaults":{"warehouse":"s3://b"},"overrides":{"prefix":"ws"}}"#;
    let (url, captured) = spawn_mock(200, body).await;
    let c = IcebergClient::new(url);
    assert_eq!(
        c.config().await.unwrap(),
        serde_json::from_str::<Value>(body).unwrap()
    );
    assert_eq!(first(&captured).path, "/v1/config");
}

#[tokio::test]
async fn non_2xx_error_carries_server_body() {
    let (url, _) = spawn_mock(404, r#"{"error":{"message":"table does not exist"}}"#).await;
    let c = IcebergClient::new(url);
    let err = c.load_table(&["ns".into()], "missing").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message().contains("table does not exist"), "got: {err}");
}

#[tokio::test]
async fn invalid_json_is_a_parse_error() {
    let (url, _) = spawn_mock(200, "not json").await;
    let c = IcebergClient::new(url);
    let err = c.list_namespaces().await.unwrap_err();
    assert!(err.message().contains("parse"), "got: {err}");
}

#[tokio::test]
async fn missing_namespaces_field_is_an_error_not_empty_list() {
    let (url, _) = spawn_mock(200, "{}").await;
    let c = IcebergClient::new(url);
    let err = c.list_namespaces().await.unwrap_err();
    assert!(err.message().contains("namespaces"), "got: {err}");
}

#[tokio::test]
async fn bearer_token_takes_precedence_over_basic() {
    let (url, captured) = spawn_mock(200, "{}").await;
    let c = client_from(&format!(
        r#"{{"base_url":"{url}","bearer_token":"secret","username":"u","password":"p"}}"#
    ));
    let _ = c.config().await;
    assert_eq!(
        first(&captured).header("authorization"),
        Some("Bearer secret")
    );
}

#[tokio::test]
async fn basic_auth_used_when_bearer_absent_and_absent_when_no_credentials() {
    let (url, captured) = spawn_mock(200, "{}").await;
    let basic = client_from(&format!(
        r#"{{"base_url":"{url}","username":"u","password":"p"}}"#
    ));
    let _ = basic.config().await;
    let anonymous = client_from(&format!(r#"{{"base_url":"{url}"}}"#));
    let _ = anonymous.config().await;

    let reqs = captured.lock().unwrap().clone();
    assert!(
        reqs[0]
            .header("authorization")
            .is_some_and(|h| h.starts_with("Basic ")),
        "got: {:?}",
        reqs[0].header("authorization")
    );
    assert_eq!(reqs[1].header("authorization"), None);
}

#[tokio::test]
async fn empty_namespace_is_rejected_before_any_request() {
    let c = IcebergClient::new("http://127.0.0.1:1");
    let err = c.list_tables(&[]).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

#[test]
fn percent_encoding_keeps_unreserved_only() {
    assert_eq!(percent_encode_segment("a b/c~d_e.f-1"), "a%20b%2Fc~d_e.f-1");
    assert_eq!(percent_encode_segment("a\u{1f}b"), "a%1Fb");
}

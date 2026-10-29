// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 端点 / 分页 / 认证 / 错误映射测试。全部对着本地 axum mock，不连真实服务。
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

fn respond(
    status: u16,
    content_type: Option<&'static str>,
    body: &'static str,
) -> axum::response::Response {
    let mut resp =
        axum::response::Response::builder().status(StatusCode::from_u16(status).unwrap());
    if let Some(ct) = content_type {
        resp = resp.header("content-type", ct);
    }
    resp.body(Body::from(body)).unwrap()
}

/// 固定状态码 + 固定响应体的 mock（无 Content-Type）。
async fn spawn_mock(status: u16, resp_body: &'static str) -> (String, Capture) {
    spawn_mock_ct(status, None, resp_body).await
}

/// 固定状态码 + Content-Type + 固定响应体的 mock。
async fn spawn_mock_ct(
    status: u16,
    content_type: Option<&'static str>,
    resp_body: &'static str,
) -> (String, Capture) {
    let captured: Capture = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::clone(&captured);
    let app = axum::Router::new().fallback(move |req: Request<Body>| {
        let state = Arc::clone(&state);
        async move {
            capture(req, &state).await;
            respond(status, content_type, resp_body)
        }
    });
    (serve(app), captured)
}

/// 按请求顺序依次返回不同响应体的 mock（分页用，无 Content-Type）。
async fn spawn_sequence(bodies: Vec<&'static str>) -> (String, Capture) {
    spawn_sequence_ct(bodies, None).await
}

/// 按请求顺序依次返回响应体、统一 Content-Type 的 mock。
async fn spawn_sequence_ct(
    bodies: Vec<&'static str>,
    content_type: Option<&'static str>,
) -> (String, Capture) {
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
            respond(200, content_type, body)
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

fn client_from(json: &str) -> DeltaSharingClient {
    DeltaSharingClient::from_config(serde_json::from_str(json).unwrap()).unwrap()
}

#[tokio::test]
async fn list_shares_gets_shares() {
    let (url, captured) = spawn_mock(200, r#"{"items":[{"name":"s1"},{"name":"s2"}]}"#).await;
    let c = DeltaSharingClient::new(url, "t");
    assert_eq!(
        c.list_shares().await.unwrap(),
        vec!["s1".to_string(), "s2".to_string()]
    );
    let req = first(&captured);
    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/shares");
    assert_eq!(req.query, "");
}

#[tokio::test]
async fn list_shares_follows_next_page_token() {
    let (url, captured) = spawn_sequence(vec![
        r#"{"items":[{"name":"s1"}],"nextPageToken":"a+b"}"#,
        r#"{"items":[{"name":"s2"}],"nextPageToken":""}"#,
    ])
    .await;
    let c = DeltaSharingClient::new(url, "t");
    assert_eq!(
        c.list_shares().await.unwrap(),
        vec!["s1".to_string(), "s2".to_string()]
    );
    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[1].query, "pageToken=a%2Bb",
        "游标必须作为 query 参数并编码"
    );
}

#[tokio::test]
async fn list_schemas_and_tables_use_nested_paths() {
    let (url, captured) = spawn_sequence(vec![
        r#"{"items":[{"name":"sc"}]}"#,
        r#"{"items":[{"name":"t1"},{"name":"t2"}]}"#,
    ])
    .await;
    let c = DeltaSharingClient::new(url, "t");
    assert_eq!(c.list_schemas("share one").await.unwrap(), vec!["sc"]);
    assert_eq!(
        c.list_tables("share one", "sch/ema").await.unwrap(),
        vec!["t1".to_string(), "t2".to_string()]
    );
    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs[0].path, "/shares/share%20one/schemas");
    assert_eq!(
        reqs[1].path, "/shares/share%20one/schemas/sch%2Fema/tables",
        "share / schema 名必须按路径段编码"
    );
}

#[tokio::test]
async fn table_metadata_returns_raw_json() {
    let body = r#"{"protocol":{"minReaderVersion":1},"metadata":{"id":"m1"}}"#;
    let (url, captured) = spawn_mock(200, body).await;
    let c = DeltaSharingClient::new(url, "t");
    assert_eq!(
        c.table_metadata("s", "sc", "tbl").await.unwrap(),
        serde_json::from_str::<Value>(body).unwrap()
    );
    assert_eq!(
        first(&captured).path,
        "/shares/s/schemas/sc/tables/tbl/metadata"
    );
}

#[tokio::test]
async fn table_version_returns_raw_json() {
    let (url, captured) = spawn_mock(200, r#"{"version":7}"#).await;
    let c = DeltaSharingClient::new(url, "t");
    assert_eq!(
        c.table_version("s", "sc", "tbl").await.unwrap(),
        json!({"version": 7})
    );
    assert_eq!(
        first(&captured).path,
        "/shares/s/schemas/sc/tables/tbl/version"
    );
}

#[tokio::test]
async fn query_table_passes_request_body_through() {
    let (url, captured) = spawn_mock(200, r#"{"files":[{"url":"https://x/f1.parquet"}]}"#).await;
    let c = DeltaSharingClient::new(url, "t");
    let request = json!({
        "predicateHints": ["date >= '2021-01-01'"],
        "jsonPredicateHints": "{}",
        "limitHint": 1000,
        "version": 3,
        "timestamp": "2021-01-01T00:00:00Z",
    });
    let out = c.query_table("s", "sc", "tbl", &request).await.unwrap();
    assert_eq!(out["files"][0]["url"], "https://x/f1.parquet");

    let req = first(&captured);
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/shares/s/schemas/sc/tables/tbl/query");
    assert_eq!(req.json(), request, "请求体必须原样透传");
}

#[tokio::test]
async fn query_table_aggregates_files_across_pages() {
    let (url, captured) = spawn_sequence(vec![
        r#"{"protocol":{"minReaderVersion":1},"metadata":{"id":"m"},
            "files":[{"url":"a"}],"nextPageToken":"tok2"}"#,
        r#"{"files":[{"url":"b"}]}"#,
    ])
    .await;
    let c = DeltaSharingClient::new(url, "t");
    let request = json!({"predicateHints": ["x = 1"]});
    let out = c.query_table("s", "sc", "tbl", &request).await.unwrap();

    assert_eq!(
        out["files"],
        json!([{"url": "a"}, {"url": "b"}]),
        "所有页的 files 必须聚合"
    );
    assert!(
        out.get("nextPageToken").is_none(),
        "分页取尽后不得留下 nextPageToken"
    );
    assert_eq!(out["protocol"]["minReaderVersion"], 1, "保留第一页的元数据");

    let reqs = captured.lock().unwrap().clone();
    assert_eq!(
        reqs[1].json(),
        json!({"predicateHints": ["x = 1"], "pageToken": "tok2"}),
        "后续页把游标放回请求体，并保留原请求字段"
    );
}

#[tokio::test]
async fn query_table_parses_ndjson_stream_and_end_stream_action() {
    let body = r#"{"protocol":{"minReaderVersion":1}}
{"metaData":{"id":"m1"}}
{"file":{"url":"https://x/a.parquet"}}
{"file":{"url":"https://x/b.parquet"}}
{"endStreamAction":{"minUrlExpirationTimestamp":123}}
"#;
    let (url, captured) =
        spawn_mock_ct(200, Some("application/x-ndjson; charset=utf-8"), body).await;
    let c = DeltaSharingClient::new(url, "t");
    let out = c.query_table("s", "sc", "tbl", &json!({})).await.unwrap();

    assert_eq!(out["protocol"]["minReaderVersion"], 1);
    assert_eq!(out["metadata"]["id"], "m1", "metaData action 放进 metadata");
    assert_eq!(
        out["files"],
        json!([
            {"url": "https://x/a.parquet"},
            {"url": "https://x/b.parquet"}
        ]),
        "所有 file action 必须聚合"
    );
    assert!(
        out.get("nextPageToken").is_none(),
        "endStreamAction 无分页字段时一次结束"
    );
    assert_eq!(
        out["minUrlExpirationTimestamp"], 123,
        "endStreamAction 的有效期要保留"
    );
    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1, "无 nextPageToken 不得续页");
    assert_eq!(reqs[0].header("authorization"), Some("Bearer t"));
    assert_eq!(
        reqs[0].header("delta-sharing-capabilities"),
        Some("includeEndStreamAction=true")
    );
}

#[tokio::test]
async fn query_table_accepts_bare_end_stream_action() {
    let body = r#"{"protocol":{"minReaderVersion":1}}
{"file":{"url":"a"}}
{"refreshToken":"rt","minUrlExpirationTimestamp":42}
"#;
    let (url, captured) = spawn_mock_ct(200, Some("application/x-ndjson"), body).await;
    let c = DeltaSharingClient::new(url, "t");
    let out = c.query_table("s", "sc", "tbl", &json!({})).await.unwrap();
    assert_eq!(out["files"], json!([{"url": "a"}]));
    assert_eq!(out["refreshToken"], "rt");
    assert_eq!(out["minUrlExpirationTimestamp"], 42);
    assert_eq!(captured.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn query_table_follows_end_stream_action_next_page_token() {
    // 第二页用 RFC 7464 的 RS (0x1E) 前缀，同时覆盖 application/json-seq。
    let (url, captured) = spawn_sequence_ct(
        vec![
            r#"{"protocol":{"minReaderVersion":1}}
{"metaData":{"id":"m"}}
{"file":{"url":"a"}}
{"endStreamAction":{"nextPageToken":"tok2","refreshToken":"rt1"}}
"#,
            "\u{1e}{\"file\":{\"url\":\"b\"}}\n\u{1e}{\"endStreamAction\":{}}\n",
        ],
        Some("application/json-seq"),
    )
    .await;
    let c = DeltaSharingClient::new(url, "t");
    let request = json!({"predicateHints": ["x = 1"]});
    let out = c.query_table("s", "sc", "tbl", &request).await.unwrap();

    assert_eq!(out["files"], json!([{"url": "a"}, {"url": "b"}]));
    assert_eq!(out["protocol"]["minReaderVersion"], 1);
    assert_eq!(out["metadata"]["id"], "m");
    assert_eq!(out["refreshToken"], "rt1", "后续页缺该字段时沿用前页的值");
    assert!(out.get("nextPageToken").is_none());

    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[1].json(),
        json!({"predicateHints": ["x = 1"], "pageToken": "tok2"}),
        "endStreamAction 的游标放回请求体，并保留原请求字段"
    );
    assert_eq!(
        reqs[1].header("delta-sharing-capabilities"),
        Some("includeEndStreamAction=true"),
        "每页请求都要带能力头"
    );
}

#[tokio::test]
async fn query_table_ndjson_without_end_stream_action_ends() {
    let body = r#"{"file":{"url":"a"}}
{"file":{"url":"b"}}
"#;
    let (url, captured) = spawn_mock_ct(200, Some("application/x-ndjson"), body).await;
    let c = DeltaSharingClient::new(url, "t");
    let out = c.query_table("s", "sc", "tbl", &json!({})).await.unwrap();
    assert_eq!(out["files"], json!([{"url": "a"}, {"url": "b"}]));
    assert_eq!(captured.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn query_table_rejects_unknown_ndjson_action() {
    let body = "{\"file\":{\"url\":\"a\"}}\n{\"queryStatus\":{\"status\":\"x\"}}\n";
    let (url, _) = spawn_mock_ct(200, Some("application/x-ndjson"), body).await;
    let c = DeltaSharingClient::new(url, "t");
    let err = c
        .query_table("s", "sc", "tbl", &json!({}))
        .await
        .unwrap_err();
    assert!(
        err.message().contains("unknown NDJSON action"),
        "未知 action 不得静默跳过，got: {err}"
    );
}

#[tokio::test]
async fn query_table_fails_on_end_stream_error_message() {
    let body = "{\"endStreamAction\":{\"errorMessage\":\"stream boom\"}}\n";
    let (url, _) = spawn_mock_ct(200, Some("application/x-ndjson"), body).await;
    let c = DeltaSharingClient::new(url, "t");
    let err = c
        .query_table("s", "sc", "tbl", &json!({}))
        .await
        .unwrap_err();
    assert!(err.message().contains("stream boom"), "got: {err}");
}

#[tokio::test]
async fn query_table_non_2xx_carries_server_body() {
    let (url, _) = spawn_mock_ct(
        400,
        Some("application/x-ndjson"),
        r#"{"errorCode":"BAD_REQUEST","message":"bad query"}"#,
    )
    .await;
    let c = DeltaSharingClient::new(url, "t");
    let err = c
        .query_table("s", "sc", "tbl", &json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message().contains("bad query"), "got: {err}");
}

#[tokio::test]
async fn non_2xx_error_carries_server_body() {
    let (url, _) = spawn_mock(
        404,
        r#"{"errorCode":"RESOURCE_DOES_NOT_EXIST","message":"no such share"}"#,
    )
    .await;
    let c = DeltaSharingClient::new(url, "t");
    let err = c.list_schemas("nope").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message().contains("no such share"), "got: {err}");
}

#[tokio::test]
async fn invalid_json_is_a_parse_error() {
    let (url, _) = spawn_mock(200, "not json").await;
    let c = DeltaSharingClient::new(url, "t");
    let err = c.list_shares().await.unwrap_err();
    assert!(err.message().contains("parse"), "got: {err}");
}

#[tokio::test]
async fn missing_items_field_is_an_error_not_empty_list() {
    let (url, _) = spawn_mock(200, "{}").await;
    let c = DeltaSharingClient::new(url, "t");
    let err = c.list_shares().await.unwrap_err();
    assert!(err.message().contains("items"), "got: {err}");
}

#[tokio::test]
async fn bearer_header_is_always_sent() {
    let (url, captured) = spawn_mock(200, r#"{"items":[]}"#).await;
    let c = client_from(&format!(
        r#"{{"base_url":"{url}","bearer_token":"secret"}}"#
    ));
    c.list_shares().await.unwrap();
    assert_eq!(
        first(&captured).header("authorization"),
        Some("Bearer secret")
    );
}

#[tokio::test]
async fn non_object_query_request_is_rejected_before_any_request() {
    let (url, captured) = spawn_mock(200, r#"{"files":[]}"#).await;
    let c = DeltaSharingClient::new(url, "t");
    let err = c
        .query_table("s", "sc", "tbl", &json!(["not-an-object"]))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(captured.lock().unwrap().is_empty(), "不得发出请求");
}

#[test]
fn percent_encoding_keeps_unreserved_only() {
    assert_eq!(percent_encode_segment("a b/c~d_e.f-1"), "a%20b%2Fc~d_e.f-1");
}

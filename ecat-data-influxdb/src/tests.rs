// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 单元测试。独立成文件：`lib.rs` 加出站韧性后会顶到 500 行硬上限。

mod resilience;

use super::*;

#[test]
fn client_constructs() {
    let _client = InfluxClient::new("http://localhost:8086", "myorg", "mybucket", "mytoken");
}

#[test]
fn data_point_builder() {
    let p = DataPoint::new("cpu")
        .with_tag("host", "server01")
        .with_field("usage", FieldValue::Float(0.85))
        .with_timestamp(1625097600000000000);
    assert_eq!(p.measurement, "cpu");
    assert_eq!(p.tags.get("host").unwrap(), "server01");
}

#[test]
fn escapes_line_parts() {
    assert_eq!(escape_line_part("a,b c=d\\e"), "a\\,b\\ c\\=d\\\\e");
    assert_eq!(escape_line_part("plain"), "plain");
}

#[test]
fn escapes_field_strings() {
    // 引号与反斜杠转义；空格、逗号在引号值内原样保留（line protocol 规范）
    assert_eq!(escape_field_string("say \"hi\""), "say \\\"hi\\\"");
    assert_eq!(escape_field_string("a\\b"), "a\\\\b");
    assert_eq!(escape_field_string("x y,z"), "x y,z");
}

/// mock InfluxDB 的 /api/v2/query 端点，返回给定状态码与错误体。
async fn spawn_mock_query(status: u16, body: &'static str) -> String {
    let app = axum::Router::new().route(
        "/api/v2/query",
        axum::routing::post(move || async move {
            (
                axum::http::StatusCode::from_u16(status).unwrap(),
                axum::response::Response::new(axum::body::Body::from(body)),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn query_returns_err_on_http_400() {
    let base_url = spawn_mock_query(400, r#"{"error":"invalid flux"}"#).await;
    let client = InfluxClient::new(base_url, "org", "bucket", "token");
    let err = client.query("from(bucket: \"x\")").await.unwrap_err();
    assert!(err.to_string().contains("invalid flux"));
}

#[derive(Clone)]
struct WriteCapture {
    path: String,
    headers: Vec<(String, String)>,
    query: Vec<(String, String)>,
    body: String,
}

impl WriteCapture {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// mock InfluxDB 的 /api/v2/write 端点：捕获请求路径/头/查询参数/体，
/// 按给定状态码与错误体应答。
async fn spawn_mock_write(
    captured: std::sync::Arc<std::sync::Mutex<Vec<WriteCapture>>>,
    status: u16,
    body: &'static str,
) -> String {
    let app = axum::Router::new().route(
        "/api/v2/write",
        axum::routing::post(
            move |req: axum::http::Request<axum::body::Body>| async move {
                let path = req.uri().path().to_string();
                let (parts, req_body) = req.into_parts();
                let headers: Vec<(String, String)> = parts
                    .headers
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.as_str().to_string(),
                            v.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                let query: Vec<(String, String)> = parts
                    .uri
                    .query()
                    .map(|q| {
                        q.split('&')
                            .filter_map(|kv| {
                                let (k, v) = kv.split_once('=')?;
                                Some((k.to_string(), v.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let req_body = axum::body::to_bytes(req_body, usize::MAX)
                    .await
                    .unwrap_or_default();
                captured
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(WriteCapture {
                        path,
                        headers,
                        query,
                        body: String::from_utf8_lossy(&req_body).into_owned(),
                    });
                if status == 200 {
                    axum::response::Response::new(axum::body::Body::from(""))
                } else {
                    use axum::response::IntoResponse;
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        axum::response::Response::new(axum::body::Body::from(body)),
                    )
                        .into_response()
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn write_builds_line_protocol_with_escaping() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let base_url = spawn_mock_write(captured.clone(), 200, "").await;
    let client = InfluxClient::new(base_url, "myorg", "mybucket", "mytoken");

    let point = DataPoint::new("cpu load")
        .with_tag("host name", "srv,1")
        .with_tag("env", "prod")
        .with_field("usage", FieldValue::Float(0.85))
        .with_field("count", FieldValue::Int(3))
        .with_field("note", FieldValue::String("say \"hi\"".into()))
        .with_field("up", FieldValue::Bool(true))
        .with_timestamp(1_700_000_000_000);
    client.write(&[point]).await.unwrap();

    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/api/v2/write");
    assert_eq!(
        reqs[0].header("authorization"),
        Some("Token mytoken"),
        "Token 认证头缺失"
    );
    // 查询参数：org/bucket/precision=ns
    let qs = &reqs[0].query;
    assert!(qs.contains(&("org".into(), "myorg".into())), "{qs:?}");
    assert!(qs.contains(&("bucket".into(), "mybucket".into())), "{qs:?}");
    assert!(qs.contains(&("precision".into(), "ns".into())), "{qs:?}");

    // 行协议：measurement 转义空格，tag 转义逗号/空格，字符串值只转义引号
    // （引号内空格合法）；tag/field 按 key 排序（BTreeMap），整行输出确定。
    assert_eq!(
        reqs[0].body,
        "cpu\\ load,env=prod,host\\ name=srv\\,1 count=3i,note=\"say \\\"hi\\\"\",up=true,usage=0.85 1700000000000\n"
    );
}

#[tokio::test]
async fn write_sends_multiple_points_as_multiple_lines() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let base_url = spawn_mock_write(captured.clone(), 200, "").await;
    let client = InfluxClient::new(base_url, "org", "bucket", "t");

    let p1 = DataPoint::new("cpu").with_field("u", FieldValue::Float(0.1));
    let p2 = DataPoint::new("mem").with_field("u", FieldValue::Float(0.2));
    client.write(&[p1, p2]).await.unwrap();

    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs.len(), 1);
    let lines: Vec<&str> = reqs[0].body.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("cpu u=0.1"));
    assert!(lines[1].starts_with("mem u=0.2"));
}

#[tokio::test]
async fn write_propagates_server_error() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let base_url = spawn_mock_write(captured.clone(), 400, "line parse error").await;
    let client = InfluxClient::new(base_url, "org", "bucket", "t");
    let err = client
        .write(&[DataPoint::new("cpu").with_field("u", FieldValue::Float(1.0))])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("line parse error"), "got: {err}");
}

#[tokio::test]
async fn query_parses_successful_json_response() {
    let body = r#"{"results":[{"series":[{"name":"cpu"}]}]}"#;
    let base_url = spawn_mock_query(200, body).await;
    let client = InfluxClient::new(base_url, "org", "bucket", "token");
    let v = client.query("from(bucket: \"x\")").await.unwrap();
    assert_eq!(v["results"][0]["series"][0]["name"], "cpu");
}

#[tokio::test]
async fn write_without_timestamp_omits_ts_suffix() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let base_url = spawn_mock_write(captured.clone(), 200, "").await;
    let client = InfluxClient::new(base_url, "org", "bucket", "t");
    client
        .write(&[DataPoint::new("cpu").with_field("u", FieldValue::Int(1))])
        .await
        .unwrap();
    let reqs = captured.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(reqs[0].body, "cpu u=1i\n");
}

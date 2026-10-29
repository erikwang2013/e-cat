// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! HBase REST 协议测试：用 axum mock 替代真实网关，不依赖 HBase。
use super::*;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Rec {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

type Log = Arc<Mutex<Vec<Rec>>>;

async fn spawn<F, Fut>(handler: F) -> String
where
    F: Fn(Request) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send + 'static,
{
    let app = axum::Router::new().fallback(handler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn read(req: Request) -> Rec {
    let (parts, body) = req.into_parts();
    let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
    Rec {
        method: parts.method,
        path: parts.uri.path().to_string(),
        headers: parts.headers,
        body,
    }
}

fn response(status: StatusCode, body: impl Into<Body>) -> Response {
    Response::builder()
        .status(status)
        .body(body.into())
        .unwrap()
}

fn header(rec: &Rec, name: &str) -> String {
    rec.headers
        .get(name)
        .unwrap_or_else(|| panic!("missing header {name}"))
        .to_str()
        .unwrap()
        .to_string()
}

/// 用给定的（明文）行键构造一个 CellSet，每行一个 `cf:v` 单元格。
fn cellset(keys: &[&str]) -> String {
    let rows: Vec<String> = keys
        .iter()
        .map(|key| {
            format!(
                r#"{{"key":"{}","Cell":[{{"column":"{}","$":"{}"}}]}}"#,
                B64.encode(key),
                B64.encode("cf:v"),
                B64.encode(key)
            )
        })
        .collect();
    format!(r#"{{"Row":[{}]}}"#, rows.join(","))
}

#[tokio::test]
async fn put_sends_base64_cellset() {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let url = spawn(move |req| {
        let seen = Arc::clone(&seen);
        async move {
            let rec = read(req).await;
            seen.lock().unwrap().push(rec);
            response(StatusCode::OK, "")
        }
    })
    .await;

    let client = HbaseClient::new(&url);
    let mut cells = BTreeMap::new();
    cells.insert("cf:name".to_string(), json!("alice"));
    cells.insert("cf:age".to_string(), json!(42));
    WideColumnClient::put(&client, "users", "row 1", &cells)
        .await
        .unwrap();

    let rec = log.lock().unwrap()[0].clone();
    assert_eq!(rec.method, Method::PUT);
    // row key 是 URL path segment：空格必须被 percent-encode。
    assert_eq!(rec.path, "/users/row%201");
    assert_eq!(header(&rec, "content-type"), "application/json");
    assert_eq!(header(&rec, "accept"), "application/json");

    let body: Value = serde_json::from_slice(&rec.body).unwrap();
    let row = &body["Row"][0];
    assert_eq!(row["key"], json!(B64.encode("row 1")));
    let sent: Vec<(String, String)> = row["Cell"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["column"].as_str().unwrap().to_string(),
                c["$"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(sent.len(), 2);
    assert!(sent.contains(&(B64.encode("cf:name"), B64.encode("alice"))));
    // 非字符串值按 serde_json 序列化（数字 42 → "42"）。
    assert!(sent.contains(&(B64.encode("cf:age"), B64.encode("42"))));
}

#[tokio::test]
async fn get_parses_cellset_values() {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let url = spawn(move |req| {
        let seen = Arc::clone(&seen);
        async move {
            let rec = read(req).await;
            seen.lock().unwrap().push(rec);
            let cell_set = r#"{"Row":[{"key":"cm93MQ==","Cell":[
                {"column":"Y2Y6YQ==","$":"djE="},
                {"column":"Y2Y6bnVt","$":"NDI="},
                {"column":"Y2Y6Ymlu","$":"//4="},
                {"column":"Y2Y6anNvbg==","$":"eyJ4IjoxfQ=="}]}]}"#;
            response(StatusCode::OK, cell_set)
        }
    })
    .await;

    let client = HbaseClient::new(&url);
    let cells = WideColumnClient::get(&client, "users", "row1")
        .await
        .unwrap()
        .expect("row exists");
    assert_eq!(cells["cf:a"], json!("v1"));
    assert_eq!(cells["cf:num"], json!(42), "能解析成 JSON 的值还原成 JSON");
    assert_eq!(cells["cf:bin"], json!("//4="), "非 UTF-8 值回 base64");
    assert_eq!(cells["cf:json"], json!({"x": 1}));
    assert_eq!(cells.len(), 4);

    let rec = log.lock().unwrap()[0].clone();
    assert_eq!(rec.method, Method::GET);
    assert_eq!(rec.path, "/users/row1");
    assert_eq!(header(&rec, "accept"), "application/json");
}

#[tokio::test]
async fn get_missing_row_returns_none() {
    let url = spawn(|_req| async { response(StatusCode::NOT_FOUND, "not found") }).await;
    let client = HbaseClient::new(&url);
    assert!(
        WideColumnClient::get(&client, "users", "absent")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn delete_sends_delete_and_tolerates_404() {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let url = spawn(move |req| {
        let seen = Arc::clone(&seen);
        async move {
            let rec = read(req).await;
            let status = if rec.path.ends_with("/absent") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            };
            seen.lock().unwrap().push(rec);
            response(status, "")
        }
    })
    .await;

    let client = HbaseClient::new(&url);
    WideColumnClient::delete(&client, "users", "row1")
        .await
        .unwrap();
    WideColumnClient::delete(&client, "users", "absent")
        .await
        .unwrap();

    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].method, Method::DELETE);
    assert_eq!(log[0].path, "/users/row1");
    assert_eq!(log[1].path, "/users/absent");
}

#[tokio::test]
async fn scan_paginates_truncates_and_closes_scanner() {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let url = spawn(move |req| {
        let seen = Arc::clone(&seen);
        async move {
            let rec = read(req).await;
            let resp = match (rec.method.as_str(), rec.path.as_str()) {
                ("PUT", "/users/scanner") => Response::builder()
                    .status(StatusCode::OK)
                    .header("Location", "/users/scanner/s1")
                    .body(Body::from(cellset(&["r1", "r2"])))
                    .unwrap(),
                ("GET", "/users/scanner/s1") => response(StatusCode::OK, cellset(&["r3", "r4"])),
                ("DELETE", "/users/scanner/s1") => response(StatusCode::OK, ""),
                _ => response(StatusCode::NOT_FOUND, ""),
            };
            seen.lock().unwrap().push(rec);
            resp
        }
    })
    .await;

    let client = HbaseClient::new(&url);
    let rows = WideColumnClient::scan(&client, "users", "pre", 3)
        .await
        .unwrap();
    let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["r1", "r2", "r3"], "达到 limit 必须截断");
    assert_eq!(rows[0].1["cf:v"], json!("r1"));

    let log = log.lock().unwrap();
    let methods: Vec<&str> = log.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(methods, ["PUT", "GET", "DELETE"], "scanner 必须被关闭");

    let scanner: Value = serde_json::from_slice(&log[0].body).unwrap();
    assert_eq!(scanner["startRow"], json!(B64.encode("pre")));
    assert_eq!(
        scanner["endRow"],
        json!(B64.encode("prf")),
        "endRow = prefix 的下一个字典序"
    );
    assert_eq!(scanner["batch"], json!(3));
}

#[tokio::test]
async fn scan_defaults_to_batch_1000_and_stops_on_204() {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let url = spawn(move |req| {
        let seen = Arc::clone(&seen);
        async move {
            let rec = read(req).await;
            let resp = match (rec.method.as_str(), rec.path.as_str()) {
                ("PUT", "/users/scanner") => Response::builder()
                    .status(StatusCode::OK)
                    .header("Location", "/users/scanner/s1")
                    .body(Body::from(cellset(&["r1"])))
                    .unwrap(),
                ("GET", "/users/scanner/s1") => response(StatusCode::NO_CONTENT, Body::empty()),
                ("DELETE", "/users/scanner/s1") => response(StatusCode::OK, ""),
                _ => response(StatusCode::NOT_FOUND, ""),
            };
            seen.lock().unwrap().push(rec);
            resp
        }
    })
    .await;

    let client = HbaseClient::new(&url);
    let rows = WideColumnClient::scan(&client, "users", "", 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let log = log.lock().unwrap();
    let scanner: Value = serde_json::from_slice(&log[0].body).unwrap();
    assert_eq!(scanner["batch"], json!(1000), "limit == 0 用默认 1000");
    assert!(scanner.get("endRow").is_none(), "空前缀没有 endRow");
}

#[tokio::test]
async fn scan_closes_scanner_when_parsing_fails() {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let url = spawn(move |req| {
        let seen = Arc::clone(&seen);
        async move {
            let rec = read(req).await;
            let resp = match (rec.method.as_str(), rec.path.as_str()) {
                ("PUT", "/users/scanner") => Response::builder()
                    .status(StatusCode::OK)
                    .header("Location", "/users/scanner/s1")
                    .body(Body::from("not json"))
                    .unwrap(),
                ("DELETE", "/users/scanner/s1") => response(StatusCode::OK, ""),
                _ => response(StatusCode::NOT_FOUND, ""),
            };
            seen.lock().unwrap().push(rec);
            resp
        }
    })
    .await;

    let client = HbaseClient::new(&url);
    let err = WideColumnClient::scan(&client, "users", "pre", 10)
        .await
        .expect_err("坏 CellSet 必须报错");
    assert!(err.message.contains("invalid CellSet"), "got: {err}");

    let log = log.lock().unwrap();
    let methods: Vec<&str> = log.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(methods, ["PUT", "DELETE"], "解析失败也要尝试关闭 scanner");
}

#[tokio::test]
async fn basic_auth_uses_authorization_header() {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let url = spawn(move |req| {
        let seen = Arc::clone(&seen);
        async move {
            let rec = read(req).await;
            seen.lock().unwrap().push(rec);
            response(StatusCode::NOT_FOUND, "")
        }
    })
    .await;

    let client = HbaseClient::with_auth(&url, "user", "pass");
    assert!(
        WideColumnClient::get(&client, "users", "r")
            .await
            .unwrap()
            .is_none()
    );
    let rec = log.lock().unwrap()[0].clone();
    assert_eq!(
        header(&rec, "authorization"),
        format!("Basic {}", B64.encode("user:pass"))
    );
}

#[test]
fn double_dot_inside_a_component_is_not_traversal() {
    assert!(validate_component("row", "a..b").is_ok());
    assert!(validate_component("row", ".").is_err());
    assert!(validate_component("row", "..").is_err());
}

#[tokio::test]
async fn rejects_unsafe_table_and_row_names() {
    // 不存在的端口：校验必须在发请求之前失败。
    let client = HbaseClient::new("http://127.0.0.1:1");
    let empty = BTreeMap::new();
    for (table, row) in [
        ("", "r"),
        ("a/b", "r"),
        ("t", ""),
        ("t", "a/b"),
        ("t", ".."),
    ] {
        let err = WideColumnClient::put(&client, table, row, &empty)
            .await
            .expect_err("非法 table/row 必须拒绝");
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{table}/{row}: {err}");
    }
    let err = WideColumnClient::scan(&client, "a/b", "", 10)
        .await
        .expect_err("非法 table 必须拒绝");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "got: {err}");

    let mut bad_cell = BTreeMap::new();
    bad_cell.insert("noqualifier".to_string(), json!(1));
    let err = WideColumnClient::put(&client, "t", "r", &bad_cell)
        .await
        .expect_err("列名必须是 family:qualifier");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "got: {err}");
}

#[tokio::test]
async fn http_errors_carry_status_and_body() {
    let url = spawn(|req| async move {
        if req.method() == Method::PUT {
            response(StatusCode::INTERNAL_SERVER_ERROR, "boom")
        } else {
            response(StatusCode::BAD_REQUEST, "bad row")
        }
    })
    .await;

    let client = HbaseClient::new(&url);
    let err = WideColumnClient::put(&client, "t", "r", &BTreeMap::new())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert!(err.message.contains("500") && err.message.contains("boom"));

    let err = WideColumnClient::get(&client, "t", "r").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument, "got: {err}");
    assert!(err.message.contains("bad row"));
}

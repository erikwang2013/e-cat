// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 协议层测试（axum mock，不依赖真实 HDFS）。韧性测试见 `tests/resilience.rs`。

mod resilience;
use super::*;
use axum::{
    Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, put},
};
use std::sync::Mutex as StdMutex;

async fn bind() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

fn serve(listener: tokio::net::TcpListener, app: Router) {
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
}

// ── CREATE：两跳，第一跳不带 body ─────────────────────────────

#[derive(Clone)]
struct PutMock {
    base: String,
    seen: Arc<StdMutex<PutSeen>>,
}

#[derive(Default)]
struct PutSeen {
    first_uri: String,
    first_body: Option<Vec<u8>>,
    second_body: Option<Vec<u8>>,
    hops: usize,
}

async fn put_first(
    State(mock): State<PutMock>,
    OriginalUri(uri): OriginalUri,
    body: Bytes,
) -> Response {
    let mut seen = mock.seen.lock().unwrap();
    seen.hops += 1;
    seen.first_uri = uri.to_string();
    seen.first_body = Some(body.to_vec());
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, format!("{}/datanode/create/1", mock.base))
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn put_second(State(mock): State<PutMock>, body: Bytes) -> StatusCode {
    let mut seen = mock.seen.lock().unwrap();
    seen.hops += 1;
    seen.second_body = Some(body.to_vec());
    StatusCode::CREATED
}

#[tokio::test]
async fn put_two_hops_first_has_no_body() {
    let (listener, base) = bind().await;
    let seen = Arc::new(StdMutex::new(PutSeen::default()));
    let mock = PutMock {
        base: base.clone(),
        seen: Arc::clone(&seen),
    };
    let app = Router::new()
        .route("/webhdfs/v1/{*path}", put(put_first))
        .route("/datanode/create/1", put(put_second))
        .with_state(mock);
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    StorageClient::put(&client, "bucket", "dir/hello world.txt", b"payload")
        .await
        .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hops, 2, "CREATE 必须恰好两跳");
    assert_eq!(
        seen.first_body.as_deref(),
        Some(&b""[..]),
        "第一跳绝不能带 body"
    );
    assert_eq!(seen.second_body.as_deref(), Some(&b"payload"[..]));
    assert!(
        seen.first_uri
            .contains("/webhdfs/v1/bucket/dir/hello%20world.txt?"),
        "路径段必须逐段编码，实际 {}",
        seen.first_uri
    );
    assert!(seen.first_uri.contains("op=CREATE"));
    assert!(seen.first_uri.contains("overwrite=true"));
    assert!(seen.first_uri.contains("user.name=ecat"));
}

/// 服务端直接 201（单跳实现）也接受。
#[tokio::test]
async fn put_accepts_direct_201() {
    async fn direct_create() -> StatusCode {
        StatusCode::CREATED
    }
    let (listener, base) = bind().await;
    let app = Router::new().route("/webhdfs/v1/{*path}", put(direct_create));
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    StorageClient::put(&client, "bucket", "key", b"x")
        .await
        .unwrap();
}

// ── OPEN：307 后读 Location 的 bytes ─────────────────────────

#[derive(Clone)]
struct OpenMock {
    base: String,
}

async fn open_first(State(mock): State<OpenMock>) -> Response {
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, format!("{}/datanode/open/1", mock.base))
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn open_second() -> &'static str {
    "hello webhdfs"
}

#[tokio::test]
async fn get_follows_307_to_datanode() {
    let (listener, base) = bind().await;
    let app = Router::new()
        .route("/webhdfs/v1/{*path}", get(open_first))
        .route("/datanode/open/1", get(open_second))
        .with_state(OpenMock { base: base.clone() });
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    let data = StorageClient::get(&client, "bucket", "key").await.unwrap();
    assert_eq!(data, b"hello webhdfs");
}

// ── LISTSTATUS：递归、只回文件、按 prefix 过滤 ───────────────

#[derive(Clone, Default)]
struct ListMock {
    queries: Arc<StdMutex<Vec<String>>>,
}

async fn list_status(State(mock): State<ListMock>, OriginalUri(uri): OriginalUri) -> Response {
    mock.queries
        .lock()
        .unwrap()
        .push(uri.query().unwrap_or_default().to_string());
    let body = match uri.path() {
        "/webhdfs/v1/bucket" => {
            r#"{"FileStatuses":{"FileStatus":[
                {"pathSuffix":"root.txt","type":"FILE"},
                {"pathSuffix":"a","type":"DIRECTORY"}]}}"#
        }
        "/webhdfs/v1/bucket/a" => {
            r#"{"FileStatuses":{"FileStatus":[{"pathSuffix":"b","type":"DIRECTORY"}]}}"#
        }
        "/webhdfs/v1/bucket/a/b" => {
            r#"{"FileStatuses":{"FileStatus":[
                {"pathSuffix":"deep.txt","type":"FILE"},
                {"pathSuffix":"skip.tmp","type":"FILE"},
                {"pathSuffix":"link","type":"SYMLINK"}]}}"#
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

#[tokio::test]
async fn list_recurses_directories_and_returns_only_files() {
    let (listener, base) = bind().await;
    let mock = ListMock::default();
    let app = Router::new().fallback(list_status).with_state(mock.clone());
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    let mut keys = StorageClient::list(&client, "bucket", "").await.unwrap();
    keys.sort();
    assert_eq!(keys, ["a/b/deep.txt", "a/b/skip.tmp", "root.txt"]);
    {
        let queries = mock.queries.lock().unwrap();
        assert_eq!(queries.len(), 3, "三个目录各一次 LISTSTATUS");
        assert!(
            queries.iter().all(|q| q.contains("op=LISTSTATUS")),
            "实际 {queries:?}"
        );
    }
    mock.queries.lock().unwrap().clear();

    let filtered = StorageClient::list(&client, "bucket", "a/b/deep")
        .await
        .unwrap();
    assert_eq!(filtered, ["a/b/deep.txt"]);
    assert_eq!(
        mock.queries.lock().unwrap().len(),
        3,
        "prefix 只做过滤，不跳过遍历"
    );
}

fn list_body(entries: impl Iterator<Item = String>) -> String {
    format!(
        r#"{{"FileStatuses":{{"FileStatus":[{}]}}}}"#,
        entries.collect::<Vec<_>>().join(",")
    )
}

async fn fixed_listing(State(body): State<Arc<String>>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.as_str().to_string(),
    )
        .into_response()
}

#[tokio::test]
async fn list_rejects_more_than_entry_cap() {
    let body = Arc::new(list_body(
        (0..=MAX_LIST_ENTRIES).map(|i| format!(r#"{{"pathSuffix":"f{i}","type":"FILE"}}"#)),
    ));
    let (listener, base) = bind().await;
    let app = Router::new().fallback(fixed_listing).with_state(body);
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    let err = StorageClient::list(&client, "bucket", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ResourceExhausted, "got: {err}");
    assert!(err.message.contains("10000"), "got: {err}");
}

#[tokio::test]
async fn list_rejects_parent_dir_suffix_from_server() {
    let body = Arc::new(list_body(std::iter::once(
        r#"{"pathSuffix":"..","type":"DIRECTORY"}"#.to_string(),
    )));
    let (listener, base) = bind().await;
    let app = Router::new().fallback(fixed_listing).with_state(body);
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    let err = StorageClient::list(&client, "bucket", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal, "got: {err}");
    assert!(err.message.contains("pathSuffix"), "got: {err}");
}

// ── DELETE ──────────────────────────────────────────────────

async fn delete_true() -> &'static str {
    r#"{"boolean":true}"#
}

async fn delete_false() -> &'static str {
    r#"{"boolean":false}"#
}

#[tokio::test]
async fn delete_parses_boolean_true() {
    let (listener, base) = bind().await;
    let app = Router::new().route("/webhdfs/v1/{*path}", delete(delete_true));
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    StorageClient::delete(&client, "bucket", "key")
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_boolean_false_is_an_error() {
    let (listener, base) = bind().await;
    let app = Router::new().route("/webhdfs/v1/{*path}", delete(delete_false));
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    let err = StorageClient::delete(&client, "bucket", "key")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal, "got: {err}");
    assert!(err.message.contains("did not delete"), "got: {err}");
}

// ── HTTP 错误与路径校验 ─────────────────────────────────────

async fn erroring(OriginalUri(uri): OriginalUri) -> Response {
    if uri.path().contains("missing") {
        (StatusCode::NOT_FOUND, "no such object").into_response()
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
    }
}

#[tokio::test]
async fn http_errors_surface_status_and_body() {
    let (listener, base) = bind().await;
    let app = Router::new().fallback(erroring);
    serve(listener, app);

    let client = HadoopClient::new(&base, "ecat");
    let err = StorageClient::get(&client, "bucket", "key")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal, "got: {err}");
    assert!(err.message.contains("HTTP 500"), "got: {err}");
    assert!(err.message.contains("boom"), "got: {err}");

    let err = StorageClient::get(&client, "bucket", "missing")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "got: {err}");
}

/// 校验在系统边界：非法输入不得到达网络（地址不可连也不影响，直接返回）。
#[tokio::test]
async fn rejects_unsafe_bucket_key_and_prefix() {
    let client = HadoopClient::new("http://127.0.0.1:1", "ecat");
    let bad: &[(&str, &str)] = &[
        ("", "key"),
        ("..", "key"),
        (".", "key"),
        ("a/b", "key"),
        ("buck\\et", "key"),
        ("bucket", ""),
        ("bucket", "/abs"),
        ("bucket", "../escape"),
        ("bucket", "a/../b"),
        ("bucket", "a//b"),
        ("bucket", "a\\b"),
    ];
    for &(bucket, key) in bad {
        let err = StorageClient::put(&client, bucket, key, b"x")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{bucket:?}/{key:?}");
        let err = StorageClient::get(&client, bucket, key).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{bucket:?}/{key:?}");
        let err = StorageClient::delete(&client, bucket, key)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{bucket:?}/{key:?}");
    }

    let err = StorageClient::list(&client, "..", "").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    let err = StorageClient::list(&client, "bucket", "../x")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

#[test]
fn encodes_each_key_segment_once() {
    let path = HadoopClient::object_path("buck et", "a b/c#d?e%f/你好").unwrap();
    assert_eq!(path, "/buck%20et/a%20b/c%23d%3Fe%25f/%E4%BD%A0%E5%A5%BD");
}

// ── 配置与认证参数 ──────────────────────────────────────────

#[test]
fn config_defaults_and_wiring() {
    let cfg: HadoopConfig =
        serde_json::from_str(r#"{"base_url":"http://127.0.0.1:9870"}"#).unwrap();
    let client = HadoopClient::from_config(cfg).unwrap();
    assert_eq!(client.user_name, "ecat");
    assert_eq!(client.delegation_token, None);
    assert_eq!(client.query_timeout, Some(Duration::from_secs(30)));
    assert_eq!(client.semaphore.as_ref().unwrap().available_permits(), 32);

    let cfg: HadoopConfig = serde_json::from_str(
        r#"{"base_url":"http://h:9870/","user_name":"alice","delegation_token":"tok",
            "query_timeout_secs":0,"max_concurrency":3,
            "breaker":{"failure_ratio":1.1}}"#,
    )
    .unwrap();
    let client = HadoopClient::from_config(cfg).unwrap();
    assert_eq!(client.base_url, "http://h:9870");
    assert_eq!(client.user_name, "alice");
    assert_eq!(client.delegation_token.as_deref(), Some("tok"));
    assert_eq!(client.query_timeout, None);
    assert_eq!(client.semaphore.as_ref().unwrap().available_permits(), 3);
}

#[tokio::test]
async fn sends_user_name_and_delegation_token() {
    #[derive(Clone, Default)]
    struct Records(Arc<StdMutex<Vec<String>>>);

    async fn ok(State(records): State<Records>, OriginalUri(uri): OriginalUri) -> &'static str {
        records.0.lock().unwrap().push(uri.to_string());
        "x"
    }

    let (listener, base) = bind().await;
    let records = Records::default();
    let app = Router::new()
        .route("/webhdfs/v1/{*path}", get(ok))
        .with_state(records.clone());
    serve(listener, app);

    let cfg: HadoopConfig = serde_json::from_str(&format!(
        r#"{{"base_url":"{base}","user_name":"alice","delegation_token":"tok en"}}"#
    ))
    .unwrap();
    let client = HadoopClient::from_config(cfg).unwrap();
    StorageClient::get(&client, "bucket", "key").await.unwrap();

    let uris = records.0.lock().unwrap();
    assert_eq!(uris.len(), 1);
    assert!(uris[0].contains("op=OPEN"), "got {}", uris[0]);
    assert!(uris[0].contains("user.name=alice"), "got {}", uris[0]);
    assert!(
        uris[0].contains("delegation=tok%20en"),
        "token 必须编码，got {}",
        uris[0]
    );
}

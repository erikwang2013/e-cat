// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::*;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use std::sync::Mutex;

mod resilience;

/// 绑定随机端口、把 URL 交给路由构造闭包，再启动 mock。
async fn spawn_with(build: impl FnOnce(&str) -> axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = build(&url);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

/// mock 一个单页响应（没有 `nextUri`）。
async fn spawn_json(body: serde_json::Value) -> String {
    let text = body.to_string();
    spawn_with(move |_| {
        axum::Router::new().route(
            "/v1/statement",
            post(move || {
                let text = text.clone();
                async move { (StatusCode::OK, text) }
            }),
        )
    })
    .await
}

/// 两页结果：第二页**省略 `columns`**，列名必须沿用第一页。
#[tokio::test]
async fn query_follows_next_uri_and_reuses_columns() {
    let url = spawn_with(|url| {
        let first = serde_json::json!({
            "id": "q1",
            "nextUri": format!("{url}/page/2"),
            "columns": [
                {"name": "id", "type": "bigint"},
                {"name": "name", "type": "varchar"},
            ],
            "data": [[1, "alice"]],
            "stats": {"processedRows": 1},
        })
        .to_string();
        axum::Router::new()
            .route(
                "/v1/statement",
                post(move || {
                    let first = first.clone();
                    async move { (StatusCode::OK, first) }
                }),
            )
            .route(
                "/page/2",
                get(|| async {
                    (
                        StatusCode::OK,
                        serde_json::json!({
                            "id": "q1",
                            "data": [[2, "bob"]],
                            "stats": {"processedRows": 2},
                        })
                        .to_string(),
                    )
                }),
            )
    })
    .await;

    let rows = TrinoClient::new(url)
        .query("select * from t")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("name"), Some(&serde_json::json!("alice")));
    assert_eq!(rows[1].get("id"), Some(&serde_json::json!(2)));
    assert_eq!(
        rows[1].columns(),
        ["id", "name"],
        "第二页省略 columns 时必须沿用第一页的列名"
    );
}

/// `execute` 返回最后一页的累计 `stats.processedRows`。
#[tokio::test]
async fn execute_returns_processed_rows_from_the_last_page() {
    let url = spawn_with(|url| {
        let first = serde_json::json!({
            "id": "q1",
            "nextUri": format!("{url}/page/2"),
            "stats": {"processedRows": 1},
        })
        .to_string();
        axum::Router::new()
            .route(
                "/v1/statement",
                post(move || {
                    let first = first.clone();
                    async move { (StatusCode::OK, first) }
                }),
            )
            .route(
                "/page/2",
                get(|| async {
                    (
                        StatusCode::OK,
                        serde_json::json!({"id": "q1", "stats": {"processedRows": 3}}).to_string(),
                    )
                }),
            )
    })
    .await;

    assert_eq!(
        TrinoClient::new(url)
            .execute("insert into t values (1), (2), (3)")
            .await
            .unwrap(),
        3
    );
}

/// 没有 `stats.processedRows` 时 `execute` 返回 0。
#[tokio::test]
async fn execute_returns_zero_without_stats() {
    let url = spawn_json(serde_json::json!({"id": "q1", "stats": {}})).await;
    assert_eq!(
        TrinoClient::new(url)
            .execute("create table t (i bigint)")
            .await
            .unwrap(),
        0
    );
}

/// HTTP 200 也可能带 `error` 对象：`message` 映射为 `Database`。
#[tokio::test]
async fn error_object_maps_to_database_error() {
    let url = spawn_json(serde_json::json!({
        "id": "q1",
        "error": {"message": "line 1:8: Table 'nope' does not exist", "errorCode": 47},
    }))
    .await;

    let err = TrinoClient::new(url)
        .query("select * from nope")
        .await
        .unwrap_err();
    assert!(matches!(err, RdbmsError::Database(_)), "got: {err:?}");
    assert!(
        err.to_string().contains("Table 'nope' does not exist"),
        "got: {err}"
    );
}

/// 非 2xx 状态把响应体原样带回。
#[tokio::test]
async fn http_error_status_reports_the_body() {
    let url = spawn_with(|_| {
        axum::Router::new().route(
            "/v1/statement",
            post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "trino is down".to_string()) }),
        )
    })
    .await;

    let err = TrinoClient::new(url).execute("select 1").await.unwrap_err();
    assert!(err.to_string().contains("trino is down"), "got: {err}");
}

/// 起始请求带上协议头：用户 / source / catalog / schema / 会话属性 / Basic 认证。
#[tokio::test]
async fn start_request_carries_trino_headers_and_basic_auth() {
    let captured: Arc<Mutex<Option<HeaderMap>>> = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&captured);
    let url = spawn_with(move |_| {
        let seen = Arc::clone(&seen);
        axum::Router::new().route(
            "/v1/statement",
            post(move |headers: HeaderMap| {
                let seen = Arc::clone(&seen);
                async move {
                    *seen.lock().unwrap() = Some(headers);
                    (StatusCode::OK, "{}".to_string())
                }
            }),
        )
    })
    .await;

    let cfg: TrinoConfig = serde_json::from_str(&format!(
        r#"{{"base_url":"{url}","user":"alice","password":"secret",
            "catalog":"hive","schema":"tpch",
            "session_properties":{{"b":"2","a":"1"}}}}"#
    ))
    .unwrap();
    TrinoClient::from_config(cfg)
        .unwrap()
        .query("select 1")
        .await
        .unwrap();

    let headers = captured
        .lock()
        .unwrap()
        .clone()
        .expect("handler 未收到请求");
    assert_eq!(headers["x-trino-user"].to_str().unwrap(), "alice");
    assert_eq!(headers["x-trino-source"].to_str().unwrap(), "ecat");
    assert_eq!(headers["x-trino-catalog"].to_str().unwrap(), "hive");
    assert_eq!(headers["x-trino-schema"].to_str().unwrap(), "tpch");
    assert_eq!(
        headers["x-trino-session"].to_str().unwrap(),
        "a=1,b=2",
        "会话属性应按 key 排序拼成 k=v,k=v"
    );
    assert_eq!(
        headers["authorization"].to_str().unwrap(),
        "Basic YWxpY2U6c2VjcmV0"
    );
}

/// `from_config` 装配可选字段；未配置 `user` 时默认 `"ecat"`，无 password 不发认证。
#[test]
fn config_wires_optional_fields_and_defaults() {
    let cfg: TrinoConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","catalog":"hive","schema":"tpch",
            "session_properties":{"x":"y"}}"#,
    )
    .unwrap();
    let c = TrinoClient::from_config(cfg).unwrap();
    assert_eq!(c.user, "ecat");
    assert!(c.password.is_none());
    assert_eq!(c.catalog.as_deref(), Some("hive"));
    assert_eq!(c.schema.as_deref(), Some("tpch"));
    assert_eq!(c.session_properties.get("x").map(String::as_str), Some("y"));
}

#[test]
fn constructors_set_user_auth_and_dialect() {
    let c = TrinoClient::with_auth("http://127.0.0.1:1", "bob", "pw");
    assert_eq!(c.user, "bob");
    assert_eq!(c.password.as_deref(), Some("pw"));
    assert_eq!(c.dialect(), Dialect::Standard);

    let c = TrinoClient::new("http://127.0.0.1:1");
    assert_eq!(c.user, "ecat");
    assert!(c.password.is_none());
}

#[tokio::test]
async fn transaction_returns_not_supported_error() {
    let client = TrinoClient::new("http://127.0.0.1:1");
    let err = match client.transaction().await {
        Err(e) => e,
        Ok(_) => panic!("expected unsupported error"),
    };
    assert!(
        err.to_string().contains("does not support transactions"),
        "got: {err}"
    );
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! DolphinScheduler 适配器的错误映射测试：HTTP 状态、JSON 解析、业务码、
//! 超时与熔断。不依赖真实 DS。

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use ecat_workflow::{RunId, WorkflowClient, WorkflowError};
use ecat_workflow_dolphinscheduler::DolphinSchedulerClient;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const PROJECT: &str = "1234567890123";
const API: &str = "/dolphinscheduler";

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn client_from(config: Value) -> DolphinSchedulerClient {
    DolphinSchedulerClient::from_config(serde_json::from_value(config).unwrap()).unwrap()
}

/// 一个只注册 `GET /projects/{PROJECT}/process-instances/{id}` 的服务。
async fn serve_status(handler: axum::routing::MethodRouter) -> DolphinSchedulerClient {
    let path = format!("{API}/projects/{PROJECT}/process-instances/{{id}}");
    let app = Router::new().route(&path, handler);
    let url = serve(app).await;
    client_from(json!({
        "base_url": format!("{url}{API}"),
        "project_code": 1234567890123_i64,
        "api_style": "legacy",
    }))
}

async fn boom() -> (StatusCode, &'static str) {
    (StatusCode::BAD_GATEWAY, "bad gateway")
}

async fn not_json() -> &'static str {
    "not json"
}

async fn business_error() -> &'static str {
    r#"{"code":50001,"msg":"process definition is not online","data":null}"#
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "{}"
}

#[tokio::test]
async fn non_2xx_maps_to_http_error() {
    let client = serve_status(get(boom)).await;

    let err = client
        .status(&RunId("9".into()))
        .await
        .expect_err("mock 恒 502");
    match err {
        WorkflowError::Http { status, body } => {
            assert_eq!(status, 502);
            assert_eq!(body, "bad gateway");
        }
        other => panic!("got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_json_maps_to_parse_error() {
    let client = serve_status(get(not_json)).await;

    let err = client
        .status(&RunId("9".into()))
        .await
        .expect_err("非 JSON 响应");
    assert!(matches!(err, WorkflowError::Parse(_)), "got {err:?}");
}

#[tokio::test]
async fn business_code_maps_to_remote_error() {
    let client = serve_status(get(business_error)).await;

    let err = client
        .status(&RunId("9".into()))
        .await
        .expect_err("DS 业务错误是 HTTP 200 + code != 0");
    assert!(
        matches!(err, WorkflowError::Remote(ref msg) if msg.contains("not online")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn timeout_maps_to_timeout() {
    let path = format!("{API}/projects/{PROJECT}/process-instances/{{id}}");
    let app = Router::new().route(&path, get(slow));
    let url = serve(app).await;
    let client = client_from(json!({
        "base_url": format!("{url}{API}"),
        "project_code": 1234567890123_i64,
        "api_style": "legacy",
        "query_timeout_secs": 1,
    }));

    // 外层保险丝：漏包超时的实现会「成功返回」而不是挂死。
    let err = tokio::time::timeout(Duration::from_secs(5), client.status(&RunId("9".into())))
        .await
        .expect("guarded 没有开火")
        .expect_err("1 秒超时对 5 秒后端必须失败");
    assert!(matches!(err, WorkflowError::Timeout(_)), "got {err:?}");
}

#[tokio::test]
async fn repeated_failures_open_the_breaker() {
    let hits = Arc::new(AtomicUsize::new(0));
    let handler = get({
        let hits = Arc::clone(&hits);
        move || {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (StatusCode::INTERNAL_SERVER_ERROR, "down")
            }
        }
    });
    let client = serve_status(handler).await;

    for _ in 0..5 {
        let err = client
            .status(&RunId("9".into()))
            .await
            .expect_err("mock 恒 500");
        assert!(
            matches!(err, WorkflowError::Http { status: 500, .. }),
            "got {err:?}"
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 5);

    let err = client
        .status(&RunId("9".into()))
        .await
        .expect_err("连续 5 次失败后熔断器必须打开");
    assert!(matches!(err, WorkflowError::Unavailable(_)), "got {err:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 5, "熔断打开后不得再打后端");
}

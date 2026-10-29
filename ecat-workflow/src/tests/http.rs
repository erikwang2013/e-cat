// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! HTTP 外壳行为测试：URL/认证/头、超时、熔断、并发上限、配置反序列化。
use crate::http::query_timeout;
use crate::{WorkflowError, WorkflowHttp, WorkflowHttpConfig};
use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// ── mock 工具 ──────────────────────────────────────────────

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 「N 个请求必须同时到齐才放行」的 mock（与 questdb resilience 同一模式）：
/// 任何并发上限 < n 的实现都凑不齐 n 个在飞请求，调用会卡到外层保险丝。
async fn spawn_barrier(n: usize) -> String {
    let barrier = Arc::new(tokio::sync::Barrier::new(n));
    let app = Router::new().fallback(move |_req: Request| {
        let barrier = Arc::clone(&barrier);
        async move {
            barrier.wait().await;
            "ok"
        }
    });
    serve(app).await
}

/// 回显 method/path/query/认证头/自定义头/body 的 mock。
async fn echo(req: Request) -> String {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let headers = req.headers().clone();
    let body = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::json!({
        "method": method,
        "path": path,
        "query": query,
        "auth": header(&headers, "authorization"),
        "api_key": header(&headers, "x-api-key"),
        "body": String::from_utf8_lossy(&body),
    })
    .to_string()
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

fn client_from(json: &str) -> WorkflowHttp {
    let cfg: WorkflowHttpConfig = serde_json::from_str(json).unwrap();
    WorkflowHttp::new(cfg).unwrap()
}

/// 一次 GET；非 2xx 转成 `Err` —— `guarded` 的熔断器只认 `Result`。
async fn get(c: &WorkflowHttp, path: &str) -> Result<(), WorkflowError> {
    c.guarded(async move {
        let resp = c
            .request(reqwest::Method::GET, path)
            .send()
            .await
            .map_err(|e| WorkflowError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(WorkflowError::Http {
                status: resp.status().as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        Ok(())
    })
    .await
}

// ── URL / 认证 / 自定义头 ──────────────────────────────────

#[tokio::test]
async fn request_builds_url_and_applies_auth_and_headers() {
    let url = serve(Router::new().fallback(echo)).await;
    // base_url 故意带末尾斜杠：必须被归一化，不能拼出 `//api`。
    let c = client_from(&format!(
        r#"{{"base_url":"{url}/","username":"user","password":"pass",
            "headers":{{"X-Api-Key":"k1"}}}}"#
    ));

    let (status, echoed) = c
        .guarded(async {
            let resp = c
                .request(reqwest::Method::POST, "/api/v1/dags?sync=true")
                .body("payload")
                .send()
                .await
                .map_err(|e| WorkflowError::Transport(e.to_string()))?;
            let status = resp.status().as_u16();
            let echoed: Value = resp
                .json()
                .await
                .map_err(|e| WorkflowError::Parse(e.to_string()))?;
            Ok::<_, WorkflowError>((status, echoed))
        })
        .await
        .unwrap();

    assert_eq!(status, 200);
    assert_eq!(echoed["method"], "POST");
    assert_eq!(echoed["path"], "/api/v1/dags");
    assert_eq!(echoed["query"], "sync=true");
    assert_eq!(echoed["auth"], "Basic dXNlcjpwYXNz");
    assert_eq!(echoed["api_key"], "k1");
    assert_eq!(echoed["body"], "payload");
    assert_eq!(c.base_url(), url, "末尾斜杠应被归一化");
}

#[tokio::test]
async fn bearer_token_wins_over_basic_credentials() {
    let url = serve(Router::new().fallback(echo)).await;
    let c = client_from(&format!(
        r#"{{"base_url":"{url}","username":"user","password":"pass","bearer_token":"tok123"}}"#
    ));
    let echoed: Value = c
        .guarded(async {
            let resp = c
                .request(reqwest::Method::GET, "/x")
                .send()
                .await
                .map_err(|e| WorkflowError::Transport(e.to_string()))?;
            resp.json()
                .await
                .map_err(|e| WorkflowError::Parse(e.to_string()))
        })
        .await
        .unwrap();
    assert_eq!(echoed["auth"], "Bearer tok123");
}

// ── 超时 ───────────────────────────────────────────────────

#[tokio::test]
async fn guarded_maps_slow_backend_to_timeout() {
    let url = serve(Router::new().fallback(|| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        "late"
    }))
    .await;
    let c = client_from(&format!(r#"{{"base_url":"{url}","query_timeout_secs":1}}"#));

    // 外层 5 秒保险丝：漏包超时的实现会「成功返回」而不是挂死。
    let err = tokio::time::timeout(Duration::from_secs(5), get(&c, "/slow"))
        .await
        .expect("guarded 没有开火（慢后端既没超时也没返回？）")
        .expect_err("1 秒超时对 5 秒后端必须失败");
    assert!(matches!(err, WorkflowError::Timeout(_)), "got: {err:?}");
}

// ── 熔断 ───────────────────────────────────────────────────

#[tokio::test]
async fn repeated_failures_open_the_breaker_and_skip_the_backend() {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback({
        let hits = Arc::clone(&hits);
        move |_req: Request| {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (StatusCode::INTERNAL_SERVER_ERROR, "down")
            }
        }
    });
    let url = serve(app).await;
    let c = client_from(&format!(r#"{{"base_url":"{url}"}}"#));

    for _ in 0..5 {
        let err = get(&c, "/x").await.expect_err("mock 恒 500");
        assert!(
            matches!(err, WorkflowError::Http { status: 500, .. }),
            "got: {err:?}"
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 5);

    let start = std::time::Instant::now();
    let err = get(&c, "/x")
        .await
        .expect_err("连续 5 次失败后熔断器必须打开");
    assert!(matches!(err, WorkflowError::Unavailable(_)), "got: {err:?}");
    assert!(err.to_string().contains("circuit breaker is open"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 5, "熔断打开后不得再打后端");
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// **超时必须在熔断内层**：超时触发的失败要计入熔断窗口。
///
/// 若把 `tokio::time::timeout` 包在 `breaker.call` 外面，超时会 drop 熔断器的
/// future，失败记录那句永远跑不到 —— 熔断器对卡死的后端永久失明，第 6 次仍会
/// 等满 1 秒超时而不是立即 `Unavailable`。这条用例就是这个顺序的探针。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker() {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback({
        let hits = Arc::clone(&hits);
        move |_req: Request| {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(5)).await;
                "late"
            }
        }
    });
    let url = serve(app).await;
    let c = client_from(&format!(r#"{{"base_url":"{url}","query_timeout_secs":1}}"#));

    for _ in 0..5 {
        let err = get(&c, "/slow")
            .await
            .expect_err("1 秒超时对 5 秒后端必须失败");
        assert!(matches!(err, WorkflowError::Timeout(_)), "got: {err:?}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 5);

    let start = std::time::Instant::now();
    let err = get(&c, "/slow")
        .await
        .expect_err("5 次超时后熔断器必须打开");
    assert!(matches!(err, WorkflowError::Unavailable(_)), "got: {err:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 5, "熔断打开后不得再打后端");
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

// ── 并发上限 ───────────────────────────────────────────────

#[tokio::test]
async fn concurrency_cap_allows_two_simultaneous_requests() {
    let url = spawn_barrier(2).await;
    let c = Arc::new(client_from(&format!(
        r#"{{"base_url":"{url}","max_concurrency":2,"query_timeout_secs":30}}"#
    )));
    assert_eq!(c.semaphore_permits(), Some(2), "2 个许可必须真的建出来");

    let mut handles = Vec::new();
    for _ in 0..2 {
        let c = Arc::clone(&c);
        handles.push(tokio::spawn(async move { get(&c, "/g").await }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个请求没到齐：并发上限挡住了第二个？"))
            .unwrap()
            .unwrap();
    }
}

/// 上限是**上界**：3 个并发同时起飞，服务端观察到的在飞数峰值必须恰好是 2
/// （=2 证明没被压到 1，≤2 证明没放开到 3）。
#[tokio::test]
async fn concurrency_cap_never_exceeds_two() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback({
        let in_flight = Arc::clone(&in_flight);
        let peak = Arc::clone(&peak);
        move |_req: Request| {
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }
    });
    let url = serve(app).await;
    let c = Arc::new(client_from(&format!(
        r#"{{"base_url":"{url}","max_concurrency":2}}"#
    )));

    let mut handles = Vec::new();
    for _ in 0..3 {
        let c = Arc::clone(&c);
        handles.push(tokio::spawn(async move { get(&c, "/g").await }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个请求挂死（许可泄漏？）"))
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "并发上限 2，实测峰值 {}",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn zero_max_concurrency_is_unlimited() {
    let url = spawn_barrier(5).await;
    let c = Arc::new(client_from(&format!(
        r#"{{"base_url":"{url}","max_concurrency":0,"query_timeout_secs":30}}"#
    )));
    assert_eq!(
        c.semaphore_permits(),
        None,
        "0 应表示不限并发（不建信号量），而不是有限上限"
    );

    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = Arc::clone(&c);
        handles.push(tokio::spawn(async move { get(&c, "/g").await }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .unwrap_or_else(|_| panic!("第 {i} 个请求没到齐：`max_concurrency: 0` 应不限并发"))
            .unwrap()
            .unwrap();
    }
}

// ── 配置反序列化 ───────────────────────────────────────────

#[test]
fn config_defaults_fill_optional_fields() {
    let cfg: WorkflowHttpConfig =
        serde_json::from_str(r#"{"base_url":"http://127.0.0.1:1"}"#).unwrap();
    assert_eq!(cfg.base_url, "http://127.0.0.1:1");
    assert!(cfg.username.is_none());
    assert!(cfg.password.is_none());
    assert!(cfg.bearer_token.is_none());
    assert!(cfg.headers.is_empty());
    assert!(cfg.tls.is_none());
    assert!(cfg.query_timeout_secs.is_none());
    assert!(cfg.breaker.is_none());
    assert!(cfg.max_concurrency.is_none());

    // 有效默认值：30 秒 / 32 并发。
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    assert_eq!(query_timeout(Some(0)), None, "0 = 显式禁用超时");
    assert_eq!(query_timeout(Some(7)), Some(Duration::from_secs(7)));
    let c = WorkflowHttp::new(cfg).unwrap();
    assert_eq!(c.semaphore_permits(), Some(32));
}

/// 适配器会用 `#[serde(flatten)]` 嵌入 `WorkflowHttpConfig` —— 包括
/// `BreakerConfig` 里用 `with = "duration_secs"` 的 `Duration` 字段也必须能解析。
#[test]
fn adapter_config_flattens_http_fields() {
    #[derive(Debug, serde::Deserialize)]
    struct AdapterConfig {
        #[serde(flatten)]
        http: WorkflowHttpConfig,
        #[serde(default)]
        project: Option<String>,
    }

    let cfg: AdapterConfig = serde_json::from_str(
        r#"{
            "base_url": "http://airflow:8080",
            "username": "u",
            "password": "p",
            "headers": {"X-Api-Key": "k"},
            "tls": {"skip_verify": true},
            "query_timeout_secs": 5,
            "max_concurrency": 4,
            "breaker": {"failure_ratio": 0.9, "window": 60},
            "project": "etl"
        }"#,
    )
    .unwrap();

    assert_eq!(cfg.http.base_url, "http://airflow:8080");
    assert_eq!(cfg.http.username.as_deref(), Some("u"));
    assert_eq!(
        cfg.http.headers.get("X-Api-Key").map(String::as_str),
        Some("k")
    );
    assert_eq!(
        cfg.http.tls.as_ref().and_then(|t| t.skip_verify),
        Some(true)
    );
    assert_eq!(cfg.http.query_timeout_secs, Some(5));
    assert_eq!(cfg.http.max_concurrency, Some(4));
    let breaker = cfg.http.breaker.as_ref().unwrap();
    assert_eq!(breaker.failure_ratio, 0.9);
    assert_eq!(breaker.window, Duration::from_secs(60));
    assert_eq!(
        cfg.project.as_deref(),
        Some("etl"),
        "适配器自有字段不受影响"
    );
}

#[test]
fn invalid_tls_config_is_a_config_error() {
    let cfg: WorkflowHttpConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","tls":{"skip_verify":true,"ca_cert":"/nonexistent"}}"#,
    )
    .unwrap();
    assert!(matches!(
        WorkflowHttp::new(cfg),
        Err(WorkflowError::Config(_))
    ));
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 基准用 HTTP 端点；`/read` 与 `/write` 走同一条 MySQL+Redis 全链路。
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ecat_metrics::MetricsLayer;
use ecat_middleware::{LoggingLayer, TracingLayer};
use serde::Deserialize;
use serde_json::json;
use tower::ServiceBuilder;

use crate::state::{self, BenchState, ROWS};

#[derive(Debug, Deserialize)]
struct ReadQuery {
    id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WriteBody {
    id: i64,
    value: String,
}

pub fn router(state: BenchState, middleware: bool) -> Router {
    let router = Router::new()
        .route("/read", get(read))
        .route("/write", post(write))
        .route("/static", get(static_ok))
        .route("/health", get(|| async { "OK" }))
        .with_state(state);
    if middleware {
        let stack = ServiceBuilder::new()
            .layer(MetricsLayer::new())
            .layer(TracingLayer::new("fullchain-bench"))
            .layer(LoggingLayer);
        router.layer(stack)
    } else {
        router
    }
}

async fn static_ok() -> &'static str {
    "OK"
}

async fn read(
    State(state): State<BenchState>,
    Query(query): Query<ReadQuery>,
) -> Result<Response, (StatusCode, String)> {
    let started = Instant::now();
    let id = query.id.unwrap_or(0).rem_euclid(ROWS);
    let (value, cache_len) = state::read(&state, id)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let mut response = Json(json!({ "value": value, "cache_len": cache_len })).into_response();
    insert_server_us(&mut response, started);
    Ok(response)
}

async fn write(
    State(state): State<BenchState>,
    Json(body): Json<WriteBody>,
) -> Result<Response, (StatusCode, String)> {
    let started = Instant::now();
    let id = body.id.rem_euclid(ROWS);
    state::write(&state, id, &body.value)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let mut response = Json(json!({ "ok": true })).into_response();
    insert_server_us(&mut response, started);
    Ok(response)
}

fn insert_server_us(response: &mut Response, started: Instant) {
    let micros = started.elapsed().as_micros().to_string();
    if let Ok(value) = HeaderValue::from_str(&micros) {
        response.headers_mut().insert("x-server-us", value);
    }
}

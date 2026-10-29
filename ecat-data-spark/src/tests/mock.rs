// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! In-process Livy mock: session/statement lifecycle plus failure and timing knobs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

pub(crate) type Mock = Arc<MockState>;

pub(crate) struct MockState {
    sessions_created: AtomicUsize,
    statements_submitted: AtomicUsize,
    session_gets: AtomicUsize,
    statement_gets: AtomicUsize,
    /// GET /sessions/{id} answers `starting` this many times per session.
    starting_polls: AtomicUsize,
    /// Per-session remaining `starting` answers.
    starting: Mutex<HashMap<i64, usize>>,
    /// GET statement answers `running` this many times per statement.
    pending_polls: AtomicUsize,
    /// Per-statement remaining `running` answers.
    pending: Mutex<HashMap<i64, usize>>,
    /// Session id reported as `dead` by GET /sessions/{id} (0 = none).
    dead_session: AtomicI64,
    output: Mutex<Value>,
    /// Non-zero = HTTP status returned instead of creating sessions/statements.
    create_status: AtomicUsize,
    statement_status: AtomicUsize,
    /// Delay applied while `slow_remaining` requests are left.
    delay_ms: AtomicU64,
    slow_remaining: AtomicU64,
    active_statements: AtomicUsize,
    peak_active_statements: AtomicUsize,
    events: Mutex<Vec<String>>,
    last_code: Mutex<String>,
    /// Session ids that statements were submitted against.
    statement_sessions: Mutex<Vec<i64>>,
    /// Bodies of POST /sessions requests.
    create_bodies: Mutex<Vec<Value>>,
}

pub(crate) fn new() -> Mock {
    Arc::new(MockState {
        sessions_created: AtomicUsize::new(0),
        statements_submitted: AtomicUsize::new(0),
        session_gets: AtomicUsize::new(0),
        statement_gets: AtomicUsize::new(0),
        starting_polls: AtomicUsize::new(1),
        starting: Mutex::new(HashMap::new()),
        pending_polls: AtomicUsize::new(1),
        pending: Mutex::new(HashMap::new()),
        dead_session: AtomicI64::new(0),
        output: Mutex::new(json!({"status": "ok", "data": {}})),
        create_status: AtomicUsize::new(0),
        statement_status: AtomicUsize::new(0),
        delay_ms: AtomicU64::new(0),
        slow_remaining: AtomicU64::new(0),
        active_statements: AtomicUsize::new(0),
        peak_active_statements: AtomicUsize::new(0),
        events: Mutex::new(Vec::new()),
        last_code: Mutex::new(String::new()),
        statement_sessions: Mutex::new(Vec::new()),
        create_bodies: Mutex::new(Vec::new()),
    })
}

impl MockState {
    pub(crate) fn sessions_created(&self) -> usize {
        self.sessions_created.load(Ordering::SeqCst)
    }

    pub(crate) fn statements_submitted(&self) -> usize {
        self.statements_submitted.load(Ordering::SeqCst)
    }

    pub(crate) fn session_gets(&self) -> usize {
        self.session_gets.load(Ordering::SeqCst)
    }

    pub(crate) fn statement_gets(&self) -> usize {
        self.statement_gets.load(Ordering::SeqCst)
    }

    pub(crate) fn peak_active_statements(&self) -> usize {
        self.peak_active_statements.load(Ordering::SeqCst)
    }

    pub(crate) fn statement_sessions(&self) -> Vec<i64> {
        self.statement_sessions.lock().unwrap().clone()
    }

    pub(crate) fn last_code(&self) -> String {
        self.last_code.lock().unwrap().clone()
    }

    pub(crate) fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    pub(crate) fn create_bodies(&self) -> Vec<Value> {
        self.create_bodies.lock().unwrap().clone()
    }

    pub(crate) fn set_output(&self, output: Value) {
        *self.output.lock().unwrap() = output;
    }

    pub(crate) fn set_starting_polls(&self, n: usize) {
        self.starting_polls.store(n, Ordering::SeqCst);
    }

    pub(crate) fn set_pending_polls(&self, n: usize) {
        self.pending_polls.store(n, Ordering::SeqCst);
    }

    pub(crate) fn set_dead_session(&self, id: i64) {
        self.dead_session.store(id, Ordering::SeqCst);
    }

    pub(crate) fn fail_statements_with(&self, status: u16) {
        self.statement_status
            .store(status as usize, Ordering::SeqCst);
    }

    pub(crate) fn fail_create_with(&self, status: u16) {
        self.create_status.store(status as usize, Ordering::SeqCst);
    }

    /// Delay the next `requests` responses by `ms` (`u64::MAX` = all of them).
    pub(crate) fn slow(&self, ms: u64, requests: u64) {
        self.delay_ms.store(ms, Ordering::SeqCst);
        self.slow_remaining.store(requests, Ordering::SeqCst);
    }

    async fn slow_response(&self) {
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay == 0 {
            return;
        }
        let slow = self
            .slow_remaining
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        if slow {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }
}

pub(crate) async fn spawn(mock: Mock) -> String {
    let app = Router::new()
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/statements", post(submit_statement))
        .route("/sessions/{id}/statements/{stmt}", get(get_statement))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn injected(status: usize, body: &'static str) -> Option<Response> {
    (status != 0).then(|| {
        (
            StatusCode::from_u16(status as u16).expect("valid injected status"),
            body,
        )
            .into_response()
    })
}

async fn create_session(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    if let Some(response) = injected(mock.create_status.load(Ordering::SeqCst), "create failed") {
        return response;
    }
    mock.create_bodies.lock().unwrap().push(body);
    mock.slow_response().await;
    let id = mock.sessions_created.fetch_add(1, Ordering::SeqCst) as i64 + 1;
    mock.starting
        .lock()
        .unwrap()
        .insert(id, mock.starting_polls.load(Ordering::SeqCst));
    Json(json!({"id": id, "state": "starting"})).into_response()
}

async fn get_session(State(mock): State<Mock>, Path(id): Path<i64>) -> Response {
    mock.session_gets.fetch_add(1, Ordering::SeqCst);
    mock.slow_response().await;
    if mock.dead_session.load(Ordering::SeqCst) == id {
        return Json(json!({"id": id, "state": "dead"})).into_response();
    }
    let starting = {
        let mut map = mock.starting.lock().unwrap();
        let left = map.entry(id).or_insert(0);
        if *left > 0 {
            *left -= 1;
            true
        } else {
            false
        }
    };
    let state = if starting { "starting" } else { "idle" };
    Json(json!({"id": id, "state": state})).into_response()
}

async fn submit_statement(
    State(mock): State<Mock>,
    Path(session_id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(response) = injected(
        mock.statement_status.load(Ordering::SeqCst),
        "statement rejected",
    ) {
        return response;
    }
    let id = mock.statements_submitted.fetch_add(1, Ordering::SeqCst) as i64 + 1;
    let code = body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Accounting happens before the optional delay so tests can observe a
    // submission while its response is still in flight.
    let active = mock.active_statements.fetch_add(1, Ordering::SeqCst) + 1;
    mock.peak_active_statements
        .fetch_max(active, Ordering::SeqCst);
    mock.events
        .lock()
        .unwrap()
        .push(format!("submit {id}: {code}"));
    *mock.last_code.lock().unwrap() = code;
    mock.statement_sessions.lock().unwrap().push(session_id);
    mock.pending
        .lock()
        .unwrap()
        .insert(id, mock.pending_polls.load(Ordering::SeqCst));
    mock.slow_response().await;
    Json(json!({"id": id, "state": "waiting"})).into_response()
}

async fn get_statement(
    State(mock): State<Mock>,
    Path((_session_id, statement_id)): Path<(i64, i64)>,
) -> Response {
    mock.statement_gets.fetch_add(1, Ordering::SeqCst);
    mock.slow_response().await;
    let outcome = {
        let mut pending = mock.pending.lock().unwrap();
        match pending.get_mut(&statement_id) {
            Some(left) if *left > 0 => {
                *left -= 1;
                Some(true) // still running
            }
            Some(_) => {
                pending.remove(&statement_id);
                Some(false) // becomes available on this poll
            }
            None => None, // already available
        }
    };
    if outcome == Some(true) {
        return Json(json!({"id": statement_id, "state": "running"})).into_response();
    }
    if outcome == Some(false) {
        mock.active_statements.fetch_sub(1, Ordering::SeqCst);
        mock.events
            .lock()
            .unwrap()
            .push(format!("available {statement_id}"));
    }
    let output = mock.output.lock().unwrap().clone();
    Json(json!({"id": statement_id, "state": "available", "output": output})).into_response()
}

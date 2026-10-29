// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::*;
use serde_json::json;

mod mock;
mod resilience;

use mock::{Mock, new, spawn};

/// Fresh mock server + client, with all lifecycle counters at zero.
async fn fresh() -> (Mock, SparkClient) {
    let mock = new();
    let url = spawn(mock.clone()).await;
    (mock, SparkClient::new(url))
}

#[test]
fn client_constructs() {
    let _client = SparkClient::new("http://localhost:8998");
}

#[test]
fn config_with_optional_auth_and_session_conf() {
    let cfg: SparkConfig = serde_json::from_str(
        r#"{"base_url":"http://localhost:8998","username":"admin","password":"pw",
            "session_conf":{"spark.executor.memory":"1g"}}"#,
    )
    .unwrap();
    let client = SparkClient::from_config(cfg).unwrap();
    assert!(client.username.is_some());
    assert_eq!(
        client.session_conf.get("spark.executor.memory"),
        Some(&"1g".to_string())
    );
}

#[test]
fn dialect_is_standard() {
    assert_eq!(SparkClient::new("http://x").dialect(), Dialect::Standard);
}

#[tokio::test]
async fn transaction_is_not_supported_without_touching_the_network() {
    let (mock, client) = fresh().await;
    let err = client
        .transaction()
        .await
        .err()
        .expect("Spark has no transactions");
    assert!(
        err.to_string().contains("does not support transactions"),
        "got: {err}"
    );
    assert_eq!(mock.sessions_created(), 0);
}

#[tokio::test]
async fn session_is_created_lazily_and_reused() {
    let (mock, client) = fresh().await;
    assert_eq!(mock.sessions_created(), 0, "construction must not do I/O");

    client.query("select 1").await.unwrap();
    client.query("select 2").await.unwrap();

    assert_eq!(
        mock.sessions_created(),
        1,
        "second statement must reuse the session"
    );
    assert_eq!(mock.statements_submitted(), 2);
    assert_eq!(mock.statement_sessions(), vec![1, 1]);
}

#[tokio::test]
async fn session_polls_until_idle() {
    let (mock, client) = fresh().await;
    mock.set_starting_polls(2);
    client.query("select 1").await.unwrap();
    assert_eq!(mock.session_gets(), 3, "starting, starting, idle");
}

#[tokio::test]
async fn statement_polls_until_available() {
    let (mock, client) = fresh().await;
    mock.set_pending_polls(2);
    let rows = client.query("select 1").await.unwrap();
    assert!(rows.is_empty());
    assert_eq!(mock.statement_gets(), 3, "running, running, available");
}

#[tokio::test]
async fn dead_session_is_cleared_and_recreated_on_the_next_call() {
    let (mock, client) = fresh().await;
    client.query("select 1").await.unwrap();
    assert_eq!(mock.sessions_created(), 1);

    mock.set_dead_session(1);
    let err = client.query("select 2").await.unwrap_err();
    assert!(err.to_string().contains("dead"), "got: {err}");
    assert_eq!(
        mock.sessions_created(),
        1,
        "a dead session must not be recreated inside the same call"
    );

    client.query("select 3").await.unwrap();
    assert_eq!(
        mock.sessions_created(),
        2,
        "the next call must rebuild the session exactly once"
    );
    assert_eq!(
        mock.statement_sessions(),
        vec![1, 2],
        "the rebuilt statement must go to the new session"
    );
}

#[tokio::test]
async fn query_parses_application_json_rows() {
    let (mock, client) = fresh().await;
    mock.set_output(json!({
        "status": "ok",
        "data": {
            "application/json":
                r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"}]"#
        }
    }));
    let rows = client.query("select * from t").await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("id"), Some(&json!(1)));
    assert_eq!(rows[0].get("name"), Some(&json!("alice")));
    assert_eq!(rows[1].get("id"), Some(&json!(2)));
}

#[tokio::test]
async fn query_parses_text_plain_output() {
    let (mock, client) = fresh().await;
    mock.set_output(json!({
        "status": "ok",
        "data": {"text/plain": "alpha\nbeta"}
    }));
    let rows = client.query("select * from t").await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("value"), Some(&json!("alpha")));
    assert_eq!(rows[1].get("value"), Some(&json!("beta")));
}

#[tokio::test]
async fn output_status_error_fails_the_query() {
    let (mock, client) = fresh().await;
    mock.set_output(json!({
        "status": "error",
        "evalue": "Table or view not found: t"
    }));
    let err = client.query("select * from t").await.unwrap_err();
    assert!(
        err.to_string().contains("Table or view not found"),
        "got: {err}"
    );
}

#[tokio::test]
async fn http_error_body_is_surfaced() {
    let (mock, client) = fresh().await;
    mock.fail_statements_with(400);
    let err = client.query("select * from t").await.unwrap_err();
    assert!(err.to_string().contains("HTTP 400"), "got: {err}");
    assert!(err.to_string().contains("statement rejected"), "got: {err}");
}

#[tokio::test]
async fn session_create_http_error_is_surfaced() {
    let (mock, client) = fresh().await;
    mock.fail_create_with(503);
    let err = client.execute("create table t (i int)").await.unwrap_err();
    assert!(err.to_string().contains("HTTP 503"), "got: {err}");
    assert!(err.to_string().contains("create failed"), "got: {err}");
}

#[tokio::test]
async fn from_config_sends_kind_and_session_conf() {
    let mock = new();
    let url = spawn(mock.clone()).await;
    let cfg: SparkConfig = serde_json::from_value(json!({
        "base_url": url,
        "session_conf": {"spark.executor.memory": "1g", "spark.driver.cores": "2"}
    }))
    .unwrap();
    let client = SparkClient::from_config(cfg).unwrap();
    client.query("select 1").await.unwrap();

    let bodies = mock.create_bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["kind"], json!("sql"));
    assert_eq!(bodies[0]["conf"]["spark.executor.memory"], json!("1g"));
    assert_eq!(bodies[0]["conf"]["spark.driver.cores"], json!("2"));
}

#[tokio::test]
async fn execute_returns_zero_and_sends_the_sql() {
    let (mock, client) = fresh().await;
    let affected = client.execute("insert into t values (1)").await.unwrap();
    assert_eq!(affected, 0, "Livy reports no affected-row count");
    assert_eq!(mock.last_code(), "insert into t values (1)");
}

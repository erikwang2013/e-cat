// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::*;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

struct EchoField;

#[async_trait::async_trait]
impl GraphQLField for EchoField {
    async fn resolve(&self, req: FieldRequest) -> Result<Value, String> {
        Ok(serde_json::json!({
            "args": req.args,
            "variables": req.variables,
            "has_selection": req.selection.is_some(),
        }))
    }
}

#[test]
fn legacy_resolver_receives_merged_args() {
    let schema = GraphQLSchema::new().query_fn("echo", |vars| Box::pin(async move { Ok(vars) }));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let vars = serde_json::json!({"id": 1});
    let data = rt
        .block_on(execute(&schema, "{ echo(id: 2, name: \"x\") }", &vars))
        .unwrap();
    assert_eq!(data["echo"], serde_json::json!({"id": 2, "name": "x"}));
}

#[test]
fn legacy_resolver_without_args_is_unchanged() {
    let schema = GraphQLSchema::new().query_fn("echo", |vars| Box::pin(async move { Ok(vars) }));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let vars = serde_json::json!({"a": 1});
    let data = rt.block_on(execute(&schema, "{ echo }", &vars)).unwrap();
    assert_eq!(data["echo"], serde_json::json!({"a": 1}));
}

#[test]
fn legacy_resolver_args_override_same_named_variables() {
    let schema = GraphQLSchema::new().query_fn("echo", |vars| Box::pin(async move { Ok(vars) }));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let vars = serde_json::json!({"id": 1});
    let data = rt
        .block_on(execute(&schema, "{ echo(id: 9) }", &vars))
        .unwrap();
    assert_eq!(data["echo"]["id"], 9);
}

#[test]
fn legacy_resolver_args_with_null_variables() {
    let schema = GraphQLSchema::new().query_fn("echo", |vars| Box::pin(async move { Ok(vars) }));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = rt
        .block_on(execute(&schema, "{ echo(id: 1) }", &Value::Null))
        .unwrap();
    assert_eq!(data["echo"], serde_json::json!({"id": 1}));
}

#[test]
fn legacy_resolver_errors_on_non_object_variables_with_args() {
    let schema = GraphQLSchema::new().query_fn("echo", |vars| Box::pin(async move { Ok(vars) }));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt
        .block_on(execute(&schema, "{ echo(id: 1) }", &serde_json::json!(5)))
        .unwrap_err();
    assert!(
        err.iter()
            .any(|e| e.contains("variables must be a JSON object")),
        "got: {err:?}"
    );
}

#[test]
fn rich_resolver_receives_full_request() {
    let schema = GraphQLSchema::new().query_field("user", EchoField);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let vars = serde_json::json!({"env": "prod"});
    let data = rt
        .block_on(execute(&schema, "{ user(id: 7) { name } }", &vars))
        .unwrap();
    // Rich resolver 收到原样 variables 与解析后的 args，二者不合并
    assert_eq!(data["user"]["args"]["id"], 7);
    assert_eq!(
        data["user"]["variables"],
        serde_json::json!({"env": "prod"})
    );
    assert_eq!(data["user"]["has_selection"], true);
}

#[test]
fn rich_resolver_without_selection() {
    let schema = GraphQLSchema::new().query_field("ping", EchoField);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = rt
        .block_on(execute(&schema, "{ ping }", &Value::Null))
        .unwrap();
    assert_eq!(data["ping"]["has_selection"], false);
    assert!(data["ping"]["args"].as_object().unwrap().is_empty());
}

#[test]
fn unknown_field_and_resolver_error_go_to_errors() {
    let schema = GraphQLSchema::new().query_field("boom", ResolveErr);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt
        .block_on(execute(&schema, "{ nope }", &Value::Null))
        .unwrap_err();
    assert!(err.iter().any(|e| e.contains("unknown field: nope")));

    let err = rt
        .block_on(execute(&schema, "{ boom }", &Value::Null))
        .unwrap_err();
    assert!(err.iter().any(|e| e == "resolver exploded"));
}

struct ResolveErr;

#[async_trait::async_trait]
impl GraphQLField for ResolveErr {
    async fn resolve(&self, _req: FieldRequest) -> Result<Value, String> {
        Err("resolver exploded".into())
    }
}

#[test]
fn mutation_dispatches_to_mutation_resolvers() {
    let schema = GraphQLSchema::new().query_fn("write", |_v| {
        Box::pin(async { Ok(serde_json::json!("query")) })
    });
    let schema = schema.mutation_field("write", MutWrite);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = rt
        .block_on(execute(&schema, "mutation { write(id: 3) }", &Value::Null))
        .unwrap();
    assert_eq!(data["write"]["id"], 3);
}

struct MutWrite;

#[async_trait::async_trait]
impl GraphQLField for MutWrite {
    async fn resolve(&self, req: FieldRequest) -> Result<Value, String> {
        Ok(serde_json::json!({"id": req.args["id"]}))
    }
}

#[test]
fn subscription_dispatches_to_query_resolvers() {
    let schema = GraphQLSchema::new().query_fn("sub", |_v| {
        Box::pin(async { Ok(serde_json::json!("query")) })
    });
    let schema = schema.mutation_field("sub", MutWrite);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = rt
        .block_on(execute(&schema, "subscription { sub }", &Value::Null))
        .unwrap();
    assert_eq!(data["sub"], "query");
}

fn router() -> Router {
    graphql_router(
        GraphQLSchema::new()
            .query_fn("hello", |_v| {
                Box::pin(async { Ok(serde_json::json!("world")) })
            })
            .query_field("user", EchoField),
    )
}

#[tokio::test]
async fn router_serves_simple_query() {
    let res = router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"{ hello }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["data"]["hello"], "world");
}

#[tokio::test]
async fn router_serves_query_with_args_and_nested_selection() {
    let res = router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"query":"{ user(id: 7, env: $e) { name } }","variables":{"e":"prod"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["data"]["user"]["args"]["id"], 7);
    assert_eq!(v["data"]["user"]["args"]["env"], "prod");
    assert_eq!(
        v["data"]["user"]["variables"],
        serde_json::json!({"e": "prod"})
    );
    assert_eq!(v["data"]["user"]["has_selection"], true);
}

#[tokio::test]
async fn router_returns_400_with_errors() {
    let res = router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"{ nope }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(v["errors"][0].as_str().unwrap().contains("unknown field"));
}

#[tokio::test]
async fn router_returns_400_on_parse_error() {
    let res = router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"{ a b }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        v["errors"][0]
            .as_str()
            .unwrap()
            .contains("multiple top-level fields")
    );
}

#[test]
fn schema_field_name_conflict_latest_wins() {
    let schema = GraphQLSchema::new()
        .query_fn("f", |_v| {
            Box::pin(async { Ok(serde_json::json!("legacy")) })
        })
        .query_field("f", EchoField);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = rt
        .block_on(execute(&schema, "{ f(a: 1) }", &Value::Null))
        .unwrap();
    assert_eq!(data["f"]["args"]["a"], 1);
}

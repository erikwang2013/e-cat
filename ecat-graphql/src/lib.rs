// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

type Resolver = Arc<
    dyn Fn(
            serde_json::Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>,
        > + Send
        + Sync,
>;

pub struct GraphQLSchema {
    query_resolvers: HashMap<String, Resolver>,
    mutation_resolvers: HashMap<String, Resolver>,
}

impl GraphQLSchema {
    pub fn new() -> Self {
        Self {
            query_resolvers: HashMap::new(),
            mutation_resolvers: HashMap::new(),
        }
    }

    pub fn query(mut self, name: impl Into<String>, r: Resolver) -> Self {
        self.query_resolvers.insert(name.into(), r);
        self
    }

    pub fn mutation(mut self, name: impl Into<String>, r: Resolver) -> Self {
        self.mutation_resolvers.insert(name.into(), r);
        self
    }

    pub fn query_fn(
        self,
        name: impl Into<String>,
        f: impl Fn(
            serde_json::Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>,
        > + Send
        + Sync
        + 'static,
    ) -> Self {
        self.query(name, Arc::new(f))
    }
}

impl Default for GraphQLSchema {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct GqlReq {
    query: String,
    #[serde(default)]
    variables: serde_json::Value,
}

pub fn graphql_router(schema: GraphQLSchema) -> Router {
    let schema = Arc::new(schema);

    async fn handler(axum::Json(req): axum::Json<GqlReq>, schema: Arc<GraphQLSchema>) -> Response {
        match execute(&schema, &req.query, &req.variables).await {
            Ok(data) => axum::Json(serde_json::json!({"data": data})).into_response(),
            Err(errors) => (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"errors": errors})),
            )
                .into_response(),
        }
    }

    let s = Arc::clone(&schema);
    Router::new().route("/graphql", post(move |body| handler(body, Arc::clone(&s))))
}

async fn execute(
    schema: &GraphQLSchema,
    query: &str,
    variables: &serde_json::Value,
) -> Result<serde_json::Value, Vec<String>> {
    let trimmed = query.trim();
    let mut errors = Vec::new();

    let (resolvers, field) = if trimmed.starts_with("mutation") {
        (&schema.mutation_resolvers, extract_field(trimmed))
    } else {
        (&schema.query_resolvers, extract_field(trimmed))
    };

    if field.is_empty() {
        errors.push("empty query field".into());
    } else if let Some(resolver) = resolvers.get(&field) {
        match resolver(variables.clone()).await {
            Ok(data) => {
                let mut result = serde_json::Map::new();
                result.insert(field, data);
                return Ok(serde_json::Value::Object(result));
            }
            Err(e) => errors.push(e),
        }
    } else {
        errors.push(format!("unknown field: {field}"));
    }

    Err(errors)
}

fn extract_field(query: &str) -> String {
    query
        .trim_start_matches("query")
        .trim_start_matches("mutation")
        .trim_start_matches(|c: char| c.is_whitespace() || c == '{')
        .trim_end_matches(|c: char| c.is_whitespace() || c == '}')
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_field_simple() {
        assert_eq!(extract_field("{ hello }"), "hello");
        assert_eq!(extract_field("query { hello }"), "hello");
        assert_eq!(extract_field("mutation { x }"), "x");
    }

    #[test]
    fn schema_and_router_builds() {
        let schema = GraphQLSchema::new().query_fn("ping", |_vars| {
            Box::pin(async { Ok(serde_json::json!("pong")) })
        });
        let _router = graphql_router(schema);
    }
}

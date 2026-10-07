// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

mod parser;
mod validation;

pub use parser::{FieldNode, Operation, SelectionSet};
pub use validation::QueryLimits;

type Resolver = Arc<
    dyn Fn(
            serde_json::Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>,
        > + Send
        + Sync,
>;

/// 富 resolver 请求上下文：字段参数、原始 variables 与嵌套 selection 树。
#[derive(Debug, Clone)]
pub struct FieldRequest {
    pub args: Map<String, Value>,
    pub variables: Value,
    pub selection: Option<SelectionSet>,
}

/// 富 resolver trait：可访问字段参数与嵌套 selection（经
/// [`GraphQLSchema::query_field`] / [`GraphQLSchema::mutation_field`] 注册）。
/// async fn in trait 非 dyn-compatible，故用 #[async_trait] 换取对象安全
/// （可存入 `FieldHandler::Rich` 的 `Arc<dyn GraphQLField>`）。
#[async_trait::async_trait]
pub trait GraphQLField: Send + Sync {
    async fn resolve(&self, req: FieldRequest) -> Result<Value, String>;
}

enum FieldHandler {
    Legacy(Resolver),
    Rich(Arc<dyn GraphQLField>),
}

pub struct GraphQLSchema {
    query_resolvers: HashMap<String, FieldHandler>,
    mutation_resolvers: HashMap<String, FieldHandler>,
    limits: QueryLimits,
}

impl GraphQLSchema {
    pub fn new() -> Self {
        Self {
            query_resolvers: HashMap::new(),
            mutation_resolvers: HashMap::new(),
            limits: QueryLimits::default(),
        }
    }

    /// 配置执行前查询预算（防查询放大 DoS），默认已启用。
    pub fn with_limits(mut self, limits: QueryLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn query(mut self, name: impl Into<String>, r: Resolver) -> Self {
        self.query_resolvers
            .insert(name.into(), FieldHandler::Legacy(r));
        self
    }

    pub fn mutation(mut self, name: impl Into<String>, r: Resolver) -> Self {
        self.mutation_resolvers
            .insert(name.into(), FieldHandler::Legacy(r));
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

    /// 注册富 query resolver：接收 `FieldRequest`（参数 + 嵌套 selection）。
    pub fn query_field(mut self, name: impl Into<String>, f: impl GraphQLField + 'static) -> Self {
        self.query_resolvers
            .insert(name.into(), FieldHandler::Rich(Arc::new(f)));
        self
    }

    /// 注册富 mutation resolver：接收 `FieldRequest`（参数 + 嵌套 selection）。
    pub fn mutation_field(
        mut self,
        name: impl Into<String>,
        f: impl GraphQLField + 'static,
    ) -> Self {
        self.mutation_resolvers
            .insert(name.into(), FieldHandler::Rich(Arc::new(f)));
        self
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

    let field = match parser::parse_query(trimmed, variables) {
        Ok(f) => f,
        Err(e) => {
            errors.push(e);
            return Err(errors);
        }
    };

    if let Err(e) = validation::validate(&field, &schema.limits) {
        errors.push(e);
        return Err(errors);
    }

    let (resolvers, field_name) = if field.operation == Operation::Mutation {
        (&schema.mutation_resolvers, &field.name)
    } else {
        (&schema.query_resolvers, &field.name)
    };

    match resolvers.get(field_name) {
        Some(FieldHandler::Legacy(resolver)) => {
            let vars = match merge_args(variables, &field.args) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(e);
                    return Err(errors);
                }
            };
            match resolver(vars).await {
                Ok(data) => {
                    let mut result = serde_json::Map::new();
                    result.insert(field.name.clone(), data);
                    return Ok(serde_json::Value::Object(result));
                }
                Err(e) => errors.push(e),
            }
        }
        Some(FieldHandler::Rich(r)) => {
            let req = FieldRequest {
                args: field.args,
                variables: variables.clone(),
                selection: field.selection,
            };
            match r.resolve(req).await {
                Ok(data) => {
                    let mut result = serde_json::Map::new();
                    result.insert(field.name.clone(), data);
                    return Ok(serde_json::Value::Object(result));
                }
                Err(e) => errors.push(e),
            }
        }
        None => errors.push(format!("unknown field: {field_name}")),
    }

    Err(errors)
}

/// Legacy resolver 的参数合并：字段参数并入 variables（同名时参数胜出）。
/// 无参数时与旧行为逐字节一致（直接克隆 variables）。
fn merge_args(variables: &Value, args: &Map<String, Value>) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(variables.clone());
    }
    match variables {
        Value::Object(m) => {
            let mut merged = m.clone();
            for (k, v) in args {
                merged.insert(k.clone(), v.clone());
            }
            Ok(Value::Object(merged))
        }
        Value::Null => Ok(Value::Object(args.clone())),
        _ => Err("variables must be a JSON object when field arguments are present".into()),
    }
}

#[cfg(test)]
mod tests;

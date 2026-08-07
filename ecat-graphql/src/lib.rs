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

    let field = match extract_field(trimmed) {
        Ok(f) => f,
        Err(e) => {
            errors.push(e);
            return Err(errors);
        }
    };

    let (resolvers, field) = if trimmed.starts_with("mutation") {
        (&schema.mutation_resolvers, field)
    } else {
        (&schema.query_resolvers, field)
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

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 跳过空白与 `#` 注释。
fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b'#') {
        if b[*i] == b'#' {
            while *i < b.len() && b[*i] != b'\n' {
                *i += 1;
            }
        } else {
            *i += 1;
        }
    }
}

/// 跳过字符串字面量（单/双引号，支持反斜杠转义与三引号块字符串）。
fn skip_string(b: &[u8], i: &mut usize) -> Result<(), String> {
    let quote = b[*i];
    *i += 1;
    // 三引号块字符串
    if *i + 1 < b.len() && b[*i] == quote && b[*i + 1] == quote {
        *i += 2;
        while *i < b.len() {
            if b[*i] == quote {
                if *i + 2 < b.len() && b[*i + 1] == quote && b[*i + 2] == quote {
                    *i += 3;
                    return Ok(());
                }
                *i += 1;
            } else {
                *i += 1;
            }
        }
        return Err("unterminated string literal".into());
    }
    while *i < b.len() {
        match b[*i] {
            b'\\' => {
                if *i + 1 < b.len() {
                    *i += 2;
                } else {
                    return Err("unterminated string literal".into());
                }
            }
            c if c == quote => {
                *i += 1;
                return Ok(());
            }
            _ => *i += 1,
        }
    }
    Err("unterminated string literal".into())
}

/// 跳过平衡的括号组；字符串内的括号/大括号不算结构字符。
fn skip_parens(b: &[u8], i: &mut usize) -> Result<(), String> {
    let mut depth = 0usize;
    while *i < b.len() {
        match b[*i] {
            b'(' => {
                depth += 1;
                *i += 1;
            }
            b')' => {
                depth -= 1;
                *i += 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            b'"' | b'\'' => skip_string(b, i)?,
            _ => *i += 1,
        }
    }
    Err("unbalanced parentheses".into())
}

/// 轻量提取首个顶层选择集的字段名。支持操作关键字、操作名、变量定义、
/// 指令与嵌套字段（括号配对、字符串常量忽略），失败返回明确错误。
fn extract_field(query: &str) -> Result<String, String> {
    let b = query.as_bytes();
    let mut i = 0;
    let n = b.len();

    skip_ws(b, &mut i);

    // 可选操作关键字：query / mutation / subscription
    for kw in ["query".as_bytes(), "mutation".as_bytes(), "subscription".as_bytes()] {
        if b[i..].starts_with(kw) {
            let after = i + kw.len();
            if after >= n || b[after].is_ascii_whitespace() || b[after] == b'(' || b[after] == b'{' {
                i = after;
                break;
            }
        }
    }
    skip_ws(b, &mut i);

    // 可选操作名
    if i < n && is_ident_byte(b[i]) {
        while i < n && is_ident_byte(b[i]) {
            i += 1;
        }
        skip_ws(b, &mut i);
    }

    // 可选变量定义（(...)）与指令（@name(...)）
    loop {
        if i < n && b[i] == b'(' {
            skip_parens(b, &mut i)?;
            skip_ws(b, &mut i);
        } else if i < n && b[i] == b'@' {
            i += 1;
            while i < n && is_ident_byte(b[i]) {
                i += 1;
            }
            skip_ws(b, &mut i);
            if i < n && b[i] == b'(' {
                skip_parens(b, &mut i)?;
                skip_ws(b, &mut i);
            }
        } else {
            break;
        }
    }

    if i >= n || b[i] != b'{' {
        return Err("expected '{' before selection".into());
    }
    i += 1;
    skip_ws(b, &mut i);

    if i + 2 < n && &b[i..i + 3] == b"..." {
        return Err("fragment spreads are not supported".into());
    }

    let start = i;
    while i < n && is_ident_byte(b[i]) {
        i += 1;
    }
    if i == start {
        return Err("expected field name".into());
    }
    Ok(String::from_utf8_lossy(&b[start..i]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_field_simple() {
        assert_eq!(extract_field("{ hello }").unwrap(), "hello");
        assert_eq!(extract_field("query { hello }").unwrap(), "hello");
        assert_eq!(extract_field("mutation { x }").unwrap(), "x");
    }

    #[test]
    fn extract_field_nested_and_args() {
        assert_eq!(
            extract_field("query GetUser { user(id: 1) { name } }").unwrap(),
            "user"
        );
        assert_eq!(
            extract_field("mutation ($v: Int!) { create(a: $v) { id } }").unwrap(),
            "create"
        );
        assert_eq!(
            extract_field("query { hello(name: \"a{b}\") }").unwrap(),
            "hello"
        );
        assert_eq!(
            extract_field("{ field(a: \"}\", b: \"(\") }").unwrap(),
            "field"
        );
        assert_eq!(
            extract_field("query @skip(if: false) { hello }").unwrap(),
            "hello"
        );
    }

    #[test]
    fn extract_field_errors_are_explicit() {
        assert!(extract_field("{").is_err());
        assert!(extract_field("").is_err());
        assert!(extract_field("query").is_err());
        assert!(extract_field("{ ...frag }").is_err());
        assert!(extract_field("{ \"not a field\" }").is_err());
    }

    #[test]
    fn extract_field_ignores_braces_in_strings() {
        // 变量定义默认值字符串里的括号/大括号不得干扰解析
        assert_eq!(
            extract_field("query ($v: String = \"a(b) { } c\") { hello }").unwrap(),
            "hello"
        );
        assert_eq!(
            extract_field("mutation { set(desc: \"a{b}c\") }").unwrap(),
            "set"
        );
    }

    #[test]
    fn schema_and_router_builds() {
        let schema = GraphQLSchema::new().query_fn("ping", |_vars| {
            Box::pin(async { Ok(serde_json::json!("pong")) })
        });
        let _router = graphql_router(schema);
    }
}

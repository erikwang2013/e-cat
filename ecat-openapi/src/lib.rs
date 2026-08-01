// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub paths: HashMap<String, PathItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Components>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenApiInfo {
    pub title: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<Operation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub responses: HashMap<String, Response>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestBody {
    pub content: HashMap<String, MediaType>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<HashMap<String, MediaType>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaType {
    pub schema: Schema,
}

#[derive(Debug, Clone, Serialize)]
pub struct Schema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, Schema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Components {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schemas: Option<HashMap<String, Schema>>,
}

pub struct OpenApiBuilder {
    title: String,
    version: String,
    paths: HashMap<String, PathItem>,
    schemas: HashMap<String, Schema>,
}

impl OpenApiBuilder {
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            paths: HashMap::new(),
            schemas: HashMap::new(),
        }
    }

    pub fn add_route(
        mut self,
        path: impl Into<String>,
        method: &str,
        summary: impl Into<String>,
        tags: Vec<String>,
    ) -> Self {
        let entry = self.paths.entry(path.into()).or_insert(PathItem {
            get: None,
            post: None,
        });
        let op = Operation {
            summary: Some(summary.into()),
            tags,
            request_body: None,
            responses: {
                let mut m = HashMap::new();
                m.insert(
                    "200".into(),
                    Response {
                        description: "Successful response".into(),
                        content: None,
                    },
                );
                m
            },
        };
        match method {
            "GET" | "get" => entry.get = Some(op),
            "POST" | "post" => entry.post = Some(op),
            _ => {}
        }
        self
    }

    pub fn add_schema(
        mut self,
        name: impl Into<String>,
        properties: HashMap<String, Schema>,
    ) -> Self {
        self.schemas.insert(
            name.into(),
            Schema {
                schema_type: Some("object".into()),
                properties: Some(properties),
                reference: None,
            },
        );
        self
    }

    pub fn build(self) -> OpenApiSpec {
        let components = if self.schemas.is_empty() {
            None
        } else {
            Some(Components {
                schemas: Some(self.schemas),
            })
        };
        OpenApiSpec {
            openapi: "3.0.3".into(),
            info: OpenApiInfo {
                title: self.title,
                version: self.version,
            },
            paths: self.paths,
            components,
        }
    }
}

pub fn schema_ref(name: &str) -> Schema {
    Schema {
        schema_type: None,
        properties: None,
        reference: Some(format!("#/components/schemas/{name}")),
    }
}

pub fn string_schema() -> Schema {
    Schema {
        schema_type: Some("string".into()),
        properties: None,
        reference: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_minimal_spec() {
        let spec = OpenApiBuilder::new("My API", "1.0.0")
            .add_route("/health", "GET", "Health check", vec!["health".into()])
            .build();
        assert_eq!(spec.openapi, "3.0.3");
    }

    #[test]
    fn build_json_output() {
        let spec = OpenApiBuilder::new("Test", "1.0").build();
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"openapi\""));
    }
}

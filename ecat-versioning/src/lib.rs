// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use axum::http::HeaderMap;
use axum::routing::get;
use std::collections::HashMap;

pub enum VersionStrategy {
    PathPrefix,
    Header,
}

pub struct VersionedRouter {
    versions: HashMap<String, axum::Router>,
    default_version: Option<String>,
    strategy: VersionStrategy,
}

impl VersionedRouter {
    pub fn new(strategy: VersionStrategy) -> Self {
        Self {
            versions: HashMap::new(),
            default_version: None,
            strategy,
        }
    }

    pub fn add_version(mut self, version: impl Into<String>, router: axum::Router) -> Self {
        self.versions.insert(version.into(), router);
        self
    }

    pub fn default_version(mut self, version: impl Into<String>) -> Self {
        self.default_version = Some(version.into());
        self
    }

    pub fn build(self) -> axum::Router {
        match self.strategy {
            VersionStrategy::PathPrefix => self.build_path_router(),
            VersionStrategy::Header => self.build_header_router(),
        }
    }

    fn build_path_router(self) -> axum::Router {
        let default = self
            .default_version
            .and_then(|v| self.versions.get(&v).cloned());
        let mut router = axum::Router::new();
        for (version, version_router) in self.versions {
            router = router.nest(&format!("/{version}"), version_router);
        }
        if let Some(default_router) = default {
            router = router.merge(default_router);
        }
        router
    }

    fn build_header_router(self) -> axum::Router {
        // Header-based routing: nest each version under the same path,
        // requiring clients to set Accept header with version param.
        let mut router = axum::Router::new();
        for (version, version_router) in self.versions {
            router = router.nest(&format!("/api"), version_router);
        }
        router
    }
}

fn extract_version(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .find(|p| p.trim().starts_with("version="))
                .map(|p| p.trim().trim_start_matches("version=").trim_matches('"').to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn health() -> &'static str {
        "ok"
    }

    #[test]
    fn path_versioned_router_builds() {
        let v1 = axum::Router::new().route("/health", get(health));
        let v2 = axum::Router::new().route("/health", get(health));
        let router = VersionedRouter::new(VersionStrategy::PathPrefix)
            .add_version("v1", v1)
            .add_version("v2", v2)
            .default_version("v1")
            .build();
        assert!(router.has_routes());
    }

    #[test]
    fn extract_version_from_accept() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Accept",
            "application/json; version=\"v2\"".parse().unwrap(),
        );
        assert_eq!(extract_version(&headers), Some("v2".into()));
    }
}

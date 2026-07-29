// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use ecat_metadata::Metadata;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct Context {
    metadata: Arc<RwLock<Metadata>>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            metadata: Arc::new(RwLock::new(Metadata::new())),
        }
    }

    pub async fn trace_id(&self) -> Option<String> {
        self.metadata.read().await.trace_id().map(|s| s.to_string())
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_context_has_no_trace_id() {
        let ctx = Context::new();
        assert!(ctx.trace_id().await.is_none());
    }

    #[tokio::test]
    async fn context_is_clonable() {
        let ctx = Context::new();
        let cloned = ctx.clone();
        assert!(cloned.trace_id().await.is_none());
    }

    #[tokio::test]
    async fn context_default_is_new() {
        let ctx: Context = Default::default();
        assert!(ctx.trace_id().await.is_none());
    }
}

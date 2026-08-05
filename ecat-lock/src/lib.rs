// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use std::time::Duration;

/// Distributed lock abstraction.
///
/// `acquire` returns an ownership token that must be passed to `release`,
/// so a lock can only be released by the process that holds it.
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// Try to acquire the lock for `key` with the given `ttl`.
    /// Returns `Some(token)` on success, `None` if the lock is held by someone else.
    async fn acquire(&self, key: &str, ttl: Duration) -> Result<Option<String>, LockError>;

    /// Release the lock for `key`, but only if `token` still matches the holder.
    async fn release(&self, key: &str, token: &str) -> Result<(), LockError>;
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("lock error: {0}")]
    Other(String),
}

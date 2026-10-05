// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;

use crate::dialect::Dialect;

#[derive(Debug, Clone)]
pub struct Row {
    columns: Vec<String>,
    values: Vec<serde_json::Value>,
}

impl Row {
    /// Create a new Row with the given columns and values.
    pub fn new(columns: Vec<String>, values: Vec<serde_json::Value>) -> Self {
        debug_assert_eq!(
            columns.len(),
            values.len(),
            "columns and values must have the same length"
        );
        Self { columns, values }
    }

    pub fn get(&self, col: &str) -> Option<&serde_json::Value> {
        self.columns
            .iter()
            .position(|c| c == col)
            .and_then(|i| self.values.get(i))
    }
}

/// Inner transaction trait for cross-backend transaction support.
#[async_trait]
pub trait TransactionInner: Send {
    async fn commit(&mut self) -> Result<(), RdbmsError>;
    async fn rollback(&mut self) -> Result<(), RdbmsError>;
}

#[derive(Default)]
pub struct Transaction {
    committed: bool,
    rolled_back: bool,
    inner: Option<Box<dyn TransactionInner>>,
}

impl Transaction {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_inner(inner: Box<dyn TransactionInner>) -> Self {
        Self {
            inner: Some(inner),
            committed: false,
            rolled_back: false,
        }
    }

    pub async fn commit(mut self) -> Result<(), RdbmsError> {
        if let Some(ref mut inner) = self.inner {
            inner.commit().await?;
        }
        self.committed = true;
        Ok(())
    }

    pub async fn rollback(mut self) -> Result<(), RdbmsError> {
        if let Some(ref mut inner) = self.inner {
            inner.rollback().await?;
        }
        self.committed = false;
        self.rolled_back = true;
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // This Drop impl only logs. No SQL is sent here (async work is not
        // possible in Drop); actual rollback relies on the backing sqlx
        // Transaction dropping without commit, which rolls back the
        // underlying DB connection.
        if !self.committed && !self.rolled_back {
            tracing::warn!("transaction dropped without commit — rolling back");
        }
    }
}

#[async_trait]
pub trait SqlExecutor: Send + Sync {
    /// 执行一条 SQL 语句，返回受影响行数。
    /// 用户提供的值请走 [`SqlExecutor::execute_with`]。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError>;
    /// 查询多行。用户提供的值请走 [`SqlExecutor::query_with`]。
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    /// 参数化执行，防注入。无法绑定参数的后端返回错误。
    async fn execute_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized execute not supported by this backend".into(),
        ))
    }
    /// 参数化查询，防注入。无法绑定参数的后端返回错误。
    async fn query_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized query not supported by this backend".into(),
        ))
    }
    /// 写路径且需要返回结果（`INSERT ... RETURNING` / `OUTPUT INSERTED`）。
    /// 默认委托给 [`SqlExecutor::query_with`]；只有读写分离路由需要覆写，
    /// 否则写语句会被路由到从库。
    async fn query_write(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.query_with(sql, params).await
    }
    /// 本执行器背后的数据库方言。
    fn dialect(&self) -> Dialect;
}

#[async_trait]
pub trait RdbmsClient: SqlExecutor {
    async fn transaction(&self) -> Result<Transaction, RdbmsError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RdbmsError {
    #[error("database error: {0}")]
    Database(String),
    #[error("connection error: {0}")]
    Connection(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("timeout: {0}")]
    Timeout(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn row_get_returns_value_by_column() {
        let row = Row::new(
            vec!["id".into(), "name".into()],
            vec![serde_json::json!(1), serde_json::json!("alice")],
        );
        assert_eq!(row.get("name"), Some(&serde_json::json!("alice")));
        assert_eq!(row.get("missing"), None);
    }

    #[test]
    fn row_get_uses_first_matching_column() {
        let row = Row::new(
            vec!["a".into(), "a".into()],
            vec![serde_json::json!(1), serde_json::json!(2)],
        );
        assert_eq!(row.get("a"), Some(&serde_json::json!(1)));
    }

    #[derive(Clone, Default)]
    struct Tracked {
        commits: Arc<AtomicUsize>,
        rollbacks: Arc<AtomicUsize>,
    }

    struct TrackingInner {
        track: Tracked,
    }

    #[async_trait]
    impl TransactionInner for TrackingInner {
        async fn commit(&mut self) -> Result<(), RdbmsError> {
            self.track.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn rollback(&mut self) -> Result<(), RdbmsError> {
            self.track.rollbacks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn commit_delegates_to_inner() {
        let track = Tracked::default();
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: track.clone(),
        }));
        tx.commit().await.unwrap();
        assert_eq!(track.commits.load(Ordering::SeqCst), 1);
        assert_eq!(track.rollbacks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rollback_delegates_to_inner() {
        let track = Tracked::default();
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: track.clone(),
        }));
        tx.rollback().await.unwrap();
        assert_eq!(track.rollbacks.load(Ordering::SeqCst), 1);
        assert_eq!(track.commits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn commit_without_inner_succeeds() {
        let tx = Transaction::new();
        tx.commit().await.unwrap();
    }

    /// 只统计 WARN 事件的最小 Subscriber，用于验证 Drop guard 的告警行为。
    #[derive(Clone)]
    struct WarnCounter(Arc<AtomicUsize>);

    impl tracing::Subscriber for WarnCounter {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    fn with_warn_counter(counts: Arc<AtomicUsize>, f: impl FnOnce()) {
        tracing::subscriber::with_default(WarnCounter(counts), f);
    }

    #[test]
    fn drop_after_explicit_rollback_does_not_warn() {
        let warns = Arc::new(AtomicUsize::new(0));
        let track = Tracked::default();
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: track.clone(),
        }));
        with_warn_counter(Arc::clone(&warns), || {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(tx.rollback())
                .unwrap();
        });
        assert_eq!(track.rollbacks.load(Ordering::SeqCst), 1);
        assert_eq!(track.commits.load(Ordering::SeqCst), 0);
        assert_eq!(
            warns.load(Ordering::SeqCst),
            0,
            "rollback 后 Drop 不得再告警"
        );
    }

    #[test]
    fn drop_without_commit_warns_once() {
        let warns = Arc::new(AtomicUsize::new(0));
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: Tracked::default(),
        }));
        with_warn_counter(Arc::clone(&warns), || drop(tx));
        assert_eq!(
            warns.load(Ordering::SeqCst),
            1,
            "未提交即 Drop 必须告警一次"
        );
    }

    struct RawOnlyClient;

    #[async_trait]
    impl SqlExecutor for RawOnlyClient {
        async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
            Ok(0)
        }
        async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![])
        }
        fn dialect(&self) -> Dialect {
            Dialect::Standard
        }
    }

    #[tokio::test]
    async fn parameterized_ops_default_to_not_supported_error() {
        let client = RawOnlyClient;
        let err = client.execute_with("SELECT 1", &[]).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("parameterized execute not supported"),
            "got: {err}"
        );
        let err = client.query_with("SELECT 1", &[]).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("parameterized query not supported"),
            "got: {err}"
        );
    }

    /// 默认的 `query_write` 必须委托给 `query_with`：这样只有读写分离路由
    /// 需要覆写它，其余后端（含第三方实现）零改动即可支持写路径。
    struct CountingClient {
        query_with_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SqlExecutor for CountingClient {
        async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
            Ok(0)
        }
        async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![])
        }
        async fn query_with(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<Vec<Row>, RdbmsError> {
            self.query_with_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        }
        fn dialect(&self) -> Dialect {
            Dialect::Standard
        }
    }

    #[tokio::test]
    async fn query_write_defaults_to_query_with() {
        let client = CountingClient {
            query_with_calls: Arc::new(AtomicUsize::new(0)),
        };
        client.query_write("SELECT 1", &[]).await.unwrap();
        assert_eq!(client.query_with_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn timeout_error_renders_message() {
        let err = RdbmsError::Timeout("query exceeded 30s".into());
        assert!(err.to_string().contains("timeout"));
        assert!(err.to_string().contains("30s"));
    }
}

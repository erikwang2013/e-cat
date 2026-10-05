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

/// 事务内部实现。后端（sqlx / tiberius）实现它，`Transaction` 转发调用。
#[async_trait]
pub trait TransactionInner: Send {
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError>;
    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    async fn execute_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError>;
    async fn query_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError>;
    fn dialect(&self) -> Dialect;
    async fn commit(&mut self) -> Result<(), RdbmsError>;
    async fn rollback(&mut self) -> Result<(), RdbmsError>;
}

/// 无 backing 连接的事务（[`Transaction::new`]）执行 SQL 时的错误文案。
/// 这类事务只能作为空占位，执行任何语句都是编程错误 —— 必须报错而非
/// 静默返回 0 行影响，否则写操作会无声丢失。
const NO_BACKING: &str = "transaction has no backing connection (created via Transaction::new)";

pub struct Transaction {
    committed: bool,
    rolled_back: bool,
    /// 在 `with_inner` 时从 inner 拷贝，避免 `dialect(&self)` 这个同步方法
    /// 需要等待异步锁。
    dialect: Dialect,
    inner: tokio::sync::Mutex<Option<Box<dyn TransactionInner>>>,
}

impl Transaction {
    /// 创建一个无 backing 连接的空事务。
    ///
    /// 只能作为占位符用于「不执行任何语句」的场景；在其中执行 SQL 会返回错误。
    /// 需要真正执行语句时用 [`Transaction::with_inner`] 或后端的 `transaction()`。
    pub fn new() -> Self {
        Self {
            committed: false,
            rolled_back: false,
            dialect: Dialect::Standard,
            inner: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_inner(inner: Box<dyn TransactionInner>) -> Self {
        let dialect = inner.dialect();
        Self {
            committed: false,
            rolled_back: false,
            dialect,
            inner: tokio::sync::Mutex::new(Some(inner)),
        }
    }

    pub async fn commit(mut self) -> Result<(), RdbmsError> {
        if let Some(inner) = self.inner.get_mut().as_mut() {
            inner.commit().await?;
        }
        self.committed = true;
        Ok(())
    }

    pub async fn rollback(mut self) -> Result<(), RdbmsError> {
        if let Some(inner) = self.inner.get_mut().as_mut() {
            inner.rollback().await?;
        }
        self.committed = false;
        self.rolled_back = true;
        Ok(())
    }
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SqlExecutor for Transaction {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.execute(sql).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.query(sql).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.execute_with(sql, params).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.query_with(sql, params).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // 这里只记日志与计数：Drop 里无法执行异步回滚，实际回滚依赖
        // 底层 sqlx / tiberius 事务在未提交时 Drop 自动回滚。
        if !self.committed && !self.rolled_back {
            crate::timeout::TRANSACTIONS_LEAKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        executes: Arc<AtomicUsize>,
    }

    struct TrackingInner {
        track: Tracked,
    }

    #[async_trait]
    impl TransactionInner for TrackingInner {
        async fn execute(&mut self, _sql: &str) -> Result<u64, RdbmsError> {
            self.track.executes.fetch_add(1, Ordering::SeqCst);
            Ok(7)
        }
        async fn query(&mut self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![Row::new(vec!["n".into()], vec![serde_json::json!(1)])])
        }
        async fn execute_with(
            &mut self,
            _sql: &str,
            _p: &[serde_json::Value],
        ) -> Result<u64, RdbmsError> {
            Ok(0)
        }
        async fn query_with(
            &mut self,
            _sql: &str,
            _p: &[serde_json::Value],
        ) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![])
        }
        fn dialect(&self) -> Dialect {
            Dialect::Sqlite
        }
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

    #[tokio::test]
    async fn transaction_executes_within_scope() {
        let track = Tracked::default();
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: track.clone(),
        }));
        assert_eq!(tx.execute("UPDATE t SET x = 1").await.unwrap(), 7);
        assert_eq!(track.executes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transaction_reports_inner_dialect() {
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: Tracked::default(),
        }));
        assert_eq!(tx.dialect(), Dialect::Sqlite);
    }

    /// 空事务执行 SQL 必须报错，而不是静默返回 0 行影响 ——
    /// 后者会让写操作无声丢失（审查发现）。
    #[tokio::test]
    async fn empty_transaction_rejects_execution() {
        let tx = Transaction::new();
        assert!(tx.execute("SELECT 1").await.is_err());
        assert!(tx.query("SELECT 1").await.is_err());
        // 没有东西要提交，不应视为错误
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
    fn dropped_uncommitted_transaction_still_warns() {
        let warns = Arc::new(AtomicUsize::new(0));
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: Tracked::default(),
        }));
        with_warn_counter(Arc::clone(&warns), || drop(tx));
        assert_eq!(warns.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dropped_uncommitted_transaction_counts_as_leak() {
        use crate::timeout::TRANSACTIONS_LEAKED;
        let before = TRANSACTIONS_LEAKED.load(Ordering::SeqCst);
        drop(Transaction::new());
        assert!(TRANSACTIONS_LEAKED.load(Ordering::SeqCst) > before); // 并行测试也会递增
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

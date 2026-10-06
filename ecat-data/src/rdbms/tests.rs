// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `rdbms.rs` 的测试。**单独成文件**是为了守住项目硬规则「每个源文件 < 500 行」——
//! 实现文件紧贴上限（同 `ecat-orm` 的 `crud.rs` + `crud/tests.rs`）。
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

/// 默认实现必须**报错**，而不是静默返回空 —— 后者会让两步式回填拿到空行、
/// 报出与真实原因无关的错（「INSERT returned no row」），把「这个后端不支持」
/// 这个真因埋掉。
#[tokio::test]
async fn execute_then_query_defaults_to_not_supported_error() {
    let client = RawOnlyClient;
    let err = client
        .execute_then_query(
            "INSERT INTO t (v) VALUES (?)",
            &[serde_json::json!("x")],
            "SELECT 1",
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("cannot run two statements atomically"),
        "got: {err}"
    );
}

#[test]
fn timeout_error_renders_message() {
    let err = RdbmsError::Timeout("query exceeded 30s".into());
    assert!(err.to_string().contains("timeout"));
    assert!(err.to_string().contains("30s"));
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerError, BreakerState};

use crate::dialect::Dialect;
use crate::rdbms::{RdbmsError, Row, SqlExecutor};

/// 给任意 [`SqlExecutor`] 包一层熔断。
///
/// **每个端点各包一个** —— 不是包在路由外层：那样单个从库故障会误熔断整条链路
/// （spec:225）。熔断打开时**内层一次都不会被调用**。
pub struct CircuitBreakerExecutor<S> {
    inner: S,
    breaker: Breaker,
}

impl<S> CircuitBreakerExecutor<S> {
    pub fn new(inner: S, cfg: BreakerConfig) -> Self {
        Self {
            inner,
            breaker: Breaker::new(cfg),
        }
    }

    /// 供 `RdbmsRouting` 跳过已熔断的端点。
    pub fn state(&self) -> BreakerState {
        self.breaker.state()
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

/// 熔断器错误的映射：后端自身的错误**原样透出** —— 熔断只决定「调不调用」，
/// 不改写后端报错。熔断打开/探测耗尽时后端根本没被调用，报「连接不可用」。
fn map_breaker_error(e: BreakerError<RdbmsError>) -> RdbmsError {
    match e {
        BreakerError::Inner(inner) => inner,
        other => RdbmsError::Connection(other.to_string()),
    }
}

#[async_trait]
impl<S: SqlExecutor> SqlExecutor for CircuitBreakerExecutor<S> {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        // 闭包**按需构造 future**：熔断打开时内层根本没被碰。
        self.breaker
            .call(|| self.inner.execute(sql))
            .await
            .map_err(map_breaker_error)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.inner.query(sql))
            .await
            .map_err(map_breaker_error)
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.breaker
            .call(|| self.inner.execute_with(sql, params))
            .await
            .map_err(map_breaker_error)
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.inner.query_with(sql, params))
            .await
            .map_err(map_breaker_error)
    }

    /// 写路径的返回行查询：同样受熔断保护 —— 熔断打开的端点连写也不该碰。
    async fn query_write(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.inner.query_write(sql, params))
            .await
            .map_err(map_breaker_error)
    }

    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[serde_json::Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.inner.execute_then_query(first, first_params, second))
            .await
            .map_err(map_breaker_error)
    }

    /// 纯本地判断，不会失败，也不该被「熔断」影响 —— 直接委托。
    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::rdbms::{RdbmsError, Row, SqlExecutor};
    use async_trait::async_trait;
    use ecat_circuit_breaker::{BreakerConfig, BreakerState};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 假执行器：**每一次真正落到内层的调用**都记一笔；
    /// `fail` 决定它报错还是成功。`dialect()` 是纯本地判断，不计入。
    struct FakeExecutor {
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    impl FakeExecutor {
        fn new(fail: bool) -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    calls: Arc::clone(&calls),
                    fail,
                },
                calls,
            )
        }

        fn record<T>(&self, ok: T) -> Result<T, RdbmsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(RdbmsError::Connection("backend down".into()))
            } else {
                Ok(ok)
            }
        }
    }

    #[async_trait]
    impl SqlExecutor for FakeExecutor {
        async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
            self.record(1)
        }
        async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            self.record(vec![])
        }
        async fn execute_with(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<u64, RdbmsError> {
            self.record(1)
        }
        async fn query_with(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<Vec<Row>, RdbmsError> {
            self.record(vec![])
        }
        async fn query_write(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<Vec<Row>, RdbmsError> {
            self.record(vec![])
        }
        async fn execute_then_query(
            &self,
            _first: &str,
            _first_params: &[serde_json::Value],
            _second: &str,
        ) -> Result<Vec<Row>, RdbmsError> {
            self.record(vec![])
        }
        fn dialect(&self) -> Dialect {
            Dialect::Sqlite
        }
    }

    /// 熔断打开后，内层**一次都不该被调用** —— 这才叫熔断，
    /// 否则只是「快速失败的转发」。
    #[tokio::test]
    async fn open_breaker_never_reaches_the_inner_executor() {
        let (inner, calls) = FakeExecutor::new(true);
        let exec = CircuitBreakerExecutor::new(inner, BreakerConfig::default());

        // 5 次失败 → 窗口总数 5、失败率 1.0 ≥ 0.5 → Open
        for _ in 0..5 {
            assert!(exec.query("SELECT 1").await.is_err());
        }
        assert_eq!(exec.state(), BreakerState::Open);

        // 清零：接下来六个方法只要有一次落到内层，计数就不再是 0
        calls.store(0, Ordering::SeqCst);
        assert!(exec.execute("UPDATE t SET x = 1").await.is_err());
        assert!(exec.query("SELECT 1").await.is_err());
        assert!(exec.execute_with("UPDATE t SET x = ?", &[]).await.is_err());
        assert!(exec.query_with("SELECT ?", &[]).await.is_err());
        assert!(
            exec.query_write("INSERT INTO t (v) VALUES (?) RETURNING id", &[])
                .await
                .is_err()
        );
        let err = exec
            .execute_then_query(
                "INSERT INTO t (v) VALUES (?)",
                &[],
                "SELECT LAST_INSERT_ID()",
            )
            .await
            .unwrap_err();
        // 熔断打开时后端根本没被调用，报「连接不可用」
        assert!(err.to_string().contains("circuit breaker"), "got: {err}");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "熔断打开后内层不得被调用（否则只是快速失败的转发）"
        );
    }

    /// 熔断只看「后端答不答」：`Ok` 记成功，后端报错记失败。
    #[tokio::test]
    async fn inner_errors_count_as_failures() {
        let (inner, calls) = FakeExecutor::new(false);
        let ok = CircuitBreakerExecutor::new(inner, BreakerConfig::default());
        for _ in 0..6 {
            ok.query_write("INSERT INTO t (v) VALUES (?) RETURNING id", &[])
                .await
                .unwrap();
        }
        assert_eq!(ok.state(), BreakerState::Closed, "成功不得触发熔断");
        assert_eq!(calls.load(Ordering::SeqCst), 6);

        let (inner, _) = FakeExecutor::new(true);
        let bad = CircuitBreakerExecutor::new(inner, BreakerConfig::default());
        for _ in 0..5 {
            assert!(bad.execute_with("UPDATE t SET x = ?", &[]).await.is_err());
        }
        assert_eq!(bad.state(), BreakerState::Open, "后端报错必须计入失败");
    }

    /// 熔断状态可被外部读取（`RdbmsRouting` 靠它跳端点）；
    /// `dialect()` 不走熔断，熔断打开也照常返回。
    #[tokio::test]
    async fn breaker_state_is_readable() {
        let (inner, _) = FakeExecutor::new(true);
        let exec = CircuitBreakerExecutor::new(inner, BreakerConfig::default());
        assert_eq!(exec.state(), BreakerState::Closed);

        for _ in 0..5 {
            assert!(exec.execute("UPDATE t SET x = 1").await.is_err());
        }
        assert_eq!(exec.state(), BreakerState::Open);
        assert_eq!(exec.dialect(), Dialect::Sqlite);
        assert_eq!(exec.inner().dialect(), Dialect::Sqlite);
    }
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! feature = "health"：池连通性探针（`SELECT 1`），接 `/health` 的 readyz。

use async_trait::async_trait;
use ecat_data::SqlExecutor;
use ecat_health::HealthCheck;
use std::sync::Arc;

/// 探针语句：只验证「池子给得出连接、库答得上话」，不碰任何业务表
/// （业务表可能还没建，那不该算不健康）。
const PROBE_SQL: &str = "SELECT 1";

/// 池连通性探针。
///
/// `C` 是任意 [`SqlExecutor`]：本 crate 的 [`crate::SqlxClient`]、或
/// `dyn RdbmsClient` 之类的 trait 对象。持 [`Arc`] 而非所有权，因为
/// `HealthRegistry::with_check` 要 `'static`，而业务侧通常也要留着同一个客户端。
///
/// ```no_run
/// # async fn f(client: std::sync::Arc<ecat_data_sqlx::SqlxClient>) {
/// use ecat_health::HealthRegistry;
/// let registry = HealthRegistry::new().with_check(
///     ecat_data_sqlx::RdbmsHealthCheck::new("db", client),
/// );
/// # let _ = registry;
/// # }
/// ```
pub struct RdbmsHealthCheck<C: ?Sized> {
    name: String,
    client: Arc<C>,
}

impl<C: SqlExecutor + ?Sized> RdbmsHealthCheck<C> {
    /// `name` 是 readyz 报告里的键名 —— 同一个 registry 里要唯一，
    /// 否则后注册的会覆盖先注册的。
    pub fn new(name: impl Into<String>, client: Arc<C>) -> Self {
        Self {
            name: name.into(),
            client,
        }
    }
}

#[async_trait]
impl<C: SqlExecutor + ?Sized> HealthCheck for RdbmsHealthCheck<C> {
    fn name(&self) -> &str {
        &self.name
    }

    /// 探通即健康。错误**原样透出**（只包一层 `to_string`）：
    /// readyz 的 `error` 字段是排障时唯一的线索，改写成「数据库不可用」
    /// 会把它抹掉。
    async fn check(&self) -> Result<(), String> {
        self.client
            .query(PROBE_SQL)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqlxClient;
    use ecat_data::{Dialect, RdbmsError, Row};
    use std::sync::Mutex;

    /// 假执行器：记下收到的 SQL，并按需报错 —— 探针「发什么语句」与
    /// 「错误怎么映射」都靠它验，不需要真库。
    struct Recorder {
        sql: Mutex<Vec<String>>,
        fail: Option<String>,
    }

    impl Recorder {
        fn new(fail: Option<&str>) -> Self {
            Self {
                sql: Mutex::new(Vec::new()),
                fail: fail.map(str::to_string),
            }
        }

        fn last_sql(&self) -> String {
            self.sql.lock().unwrap().last().cloned().unwrap_or_default()
        }
    }

    #[async_trait]
    impl SqlExecutor for Recorder {
        async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
            self.sql.lock().unwrap().push(sql.to_string());
            Err(RdbmsError::Database("execute 不该被探针调用".into()))
        }

        async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
            self.sql.lock().unwrap().push(sql.to_string());
            match &self.fail {
                Some(msg) => Err(RdbmsError::Connection(msg.clone())),
                None => Ok(vec![Row::new(vec!["1".into()], vec![1.into()])]),
            }
        }

        fn dialect(&self) -> Dialect {
            Dialect::Standard
        }
    }

    /// 探针语句必须是 `SELECT 1`：真拿到行才算通，`execute` 一条 DDL 是测不出
    /// 「库答得上话」的（而且 `execute` 被调用本身就是回归）。
    #[tokio::test]
    async fn probe_runs_select_1_and_passes() {
        let recorder = Arc::new(Recorder::new(None));
        let check = RdbmsHealthCheck::new("db", Arc::clone(&recorder));

        assert_eq!(check.name(), "db");
        assert_eq!(check.check().await, Ok(()));
        assert_eq!(recorder.last_sql(), "SELECT 1");
    }

    /// 不健康时把后端的错误原样带出来（不是一句笼统的「数据库不可用」）。
    #[tokio::test]
    async fn failure_surfaces_backend_error_verbatim() {
        let recorder = Arc::new(Recorder::new(Some("connection refused")));
        let check = RdbmsHealthCheck::new("db", recorder);

        let err = check.check().await.unwrap_err();
        assert_eq!(err, "connection error: connection refused");
    }

    /// 真客户端（SQLite 内存库）走同一条路径。
    #[tokio::test]
    async fn real_sqlite_client_is_healthy() {
        let client = Arc::new(SqlxClient::connect("sqlite::memory:").await.unwrap());
        let check = RdbmsHealthCheck::new("sqlite", client);
        assert_eq!(check.check().await, Ok(()));
    }
}

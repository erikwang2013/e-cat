// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
mod cell;
mod config;
#[cfg(feature = "health")]
mod health;
#[cfg(test)]
mod live_tests;
#[cfg(feature = "metrics")]
mod metrics;
mod pool;
#[cfg(test)]
mod tests;
// **不**随 feature 门控：feature 关闭时 `timed` 是直通函数（见模块文档），
// 这样每个调用点不必各写一次 `#[cfg]`。
mod tracing;
mod transaction;

use async_trait::async_trait;
use cell::{mysql_rows_to_result, pg_rows_to_result, sqlite_rows_to_result};
use ecat_data::{
    Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner,
    run_with_timeout,
};
use std::time::Duration;
use transaction::{MyTx, PgTx, SqTx};

pub use config::{PoolParams, SqlxConfig};
#[cfg(feature = "health")]
pub use health::RdbmsHealthCheck;
#[cfg(feature = "metrics")]
pub use metrics::register_pool_metrics;
pub use pool::{Pool, PoolGuard};

fn percent_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ':' | '/' | '@' | '#' | '?' | '&' | '=' | '%' | '+' | ' ' => {
                format!("%{:02X}", c as u8)
            }
            _ => c.to_string(),
        })
        .collect()
}

/// 把账号密码嵌入 URL（已有 `@` 时原样返回）。
fn with_auth_in_url(url: &str, username: &str, password: &str) -> String {
    if url.contains('@') {
        return url.to_string();
    }
    let encoded_user = percent_encode(username);
    let encoded_pass = percent_encode(password);
    url.replacen("://", &format!("://{encoded_user}:{encoded_pass}@"), 1)
}

/// sqlx 错误 → [`RdbmsError::Database`]。三路分派后每个方法都有三个分支，
/// 统一走这里避免 6 处重复。
///
/// 也是「等连接超时」的唯一漏斗：`PoolTimedOut` 由 sqlx 在内部取用连接超时时
/// 抛出，流经此处时记一笔（`metrics` feature 的
/// `ecat_rdbms_pool_timeouts_total`）。
fn db_err(e: sqlx::Error) -> RdbmsError {
    #[cfg(feature = "metrics")]
    if matches!(e, sqlx::Error::PoolTimedOut) {
        metrics::count_pool_timeout();
    }
    RdbmsError::Database(e.to_string())
}

pub struct SqlxClient {
    pool: Pool,
    query_timeout: Option<Duration>,
    min_connections: u32,
    /// 慢查询告警阈值；`None` = 不打。只有 `tracing` feature 会读它，
    /// 但字段本身常驻 —— 否则配置项会随 feature 时有时无（见 [`SqlxConfig`]）。
    slow_query: Option<Duration>,
}

impl SqlxClient {
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        // 必须走 for_url 而非 default()：否则 connect() 拿不到方言默认的
        // session_init，与 from_config() 行为静默不一致。
        Self::connect_with_params(url, &PoolParams::for_url(url)).await
    }

    pub async fn connect_with_params(url: &str, params: &PoolParams) -> Result<Self, sqlx::Error> {
        Ok(Self {
            pool: Pool::connect(url, params).await?,
            query_timeout: params.query_timeout,
            min_connections: params.min_connections,
            slow_query: params.slow_query,
        })
    }

    pub async fn connect_with_auth(
        url: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, sqlx::Error> {
        Self::connect(&with_auth_in_url(url, username, password)).await
    }

    pub async fn from_config(cfg: SqlxConfig) -> Result<Self, sqlx::Error> {
        // 响亮失败而非静默忽略：本字段曾被 serde 接受却从不读，
        // 用户以为开了 TLS 实际什么都没发生。
        if cfg.tls.is_some() {
            return Err(sqlx::Error::Configuration(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SqlxConfig.tls is not supported: configure TLS through URL parameters \
                 instead (e.g. postgres://...?sslmode=require)",
            ))));
        }
        let params = cfg.pool();
        let url = match (&cfg.username, &cfg.password) {
            (Some(u), Some(p)) if !u.is_empty() || !p.is_empty() => {
                with_auth_in_url(&cfg.url, u, p)
            }
            _ => cfg.url.clone(),
        };
        Self::connect_with_params(&url, &params).await
    }

    /// 用已有原生池构造客户端。
    ///
    /// 拿的是**裸池**，没有 `SqlxConfig` 可读，因此查询超时与 [`SqlxClient::warm_up`]
    /// 在这里都是关的（`query_timeout: None`、`min_connections: 0`）——
    /// 这是 API 形状决定的，不是遗漏。需要两者请用
    /// [`SqlxClient::connect_with_params`] 或 [`SqlxClient::from_config`]。
    pub fn from_pool(pool: Pool) -> Self {
        Self {
            pool,
            query_timeout: None,
            min_connections: 0,
            slow_query: None,
        }
    }

    /// 池本身（`metrics` feature 的 [`register_pool_metrics`] 要它）。
    /// sqlx 的池内部是 `Arc`，克隆很轻。
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// 一次数据库调用的公共外壳：查询超时 + 慢查询告警。
    /// 接在此处而不是各方法里，是为了两件事都只有一处定义。
    async fn timed<F, T>(&self, sql: &str, fut: F) -> Result<T, RdbmsError>
    where
        F: std::future::Future<Output = Result<T, RdbmsError>>,
    {
        crate::tracing::timed(
            self.slow_query,
            sql,
            run_with_timeout(self.query_timeout, fut),
        )
        .await
    }

    pub fn dialect(&self) -> Dialect {
        self.pool.dialect()
    }

    /// 池内已建立的连接总数（供 metrics 与测试）。
    pub fn pool_size(&self) -> u32 {
        self.pool.size()
    }

    /// 同步建满 `min_connections` 条连接并归还，让服务启动后立刻处于就绪态。
    /// 在服务启动时调用一次。
    ///
    /// 为什么 sqlx 已有 `min_connections` 还需要它：sqlx 的保底由**后台任务
    /// 异步维护**（`sqlx-core-0.8.6/src/pool/inner.rs:514-517`），`connect()`
    /// 返回时不保证已建满 —— 启动后第一波请求会与后台任务抢跑。
    pub async fn warm_up(&self) -> Result<(), RdbmsError> {
        let mut guards = Vec::with_capacity(self.min_connections as usize);
        while (guards.len() as u32) < self.min_connections {
            guards.push(
                self.pool
                    .acquire()
                    .await
                    .map_err(|e| RdbmsError::Connection(e.to_string()))?,
            );
        }
        // guards 在此处 drop，连接归还池中
        Ok(())
    }
}

#[async_trait]
impl SqlExecutor for SqlxClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.timed(sql, async {
            let affected = match &self.pool {
                Pool::Pg(p) => sqlx::query(sql).execute(p).await.map(|r| r.rows_affected()),
                Pool::My(p) => sqlx::query(sql).execute(p).await.map(|r| r.rows_affected()),
                Pool::Sq(p) => sqlx::query(sql).execute(p).await.map(|r| r.rows_affected()),
            }
            .map_err(db_err)?;
            Ok(affected)
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.timed(sql, async {
            let rows = match &self.pool {
                Pool::Pg(p) => sqlx::query(sql)
                    .fetch_all(p)
                    .await
                    .map_err(db_err)
                    .and_then(pg_rows_to_result),
                Pool::My(p) => sqlx::query(sql)
                    .fetch_all(p)
                    .await
                    .map_err(db_err)
                    .and_then(mysql_rows_to_result),
                Pool::Sq(p) => sqlx::query(sql)
                    .fetch_all(p)
                    .await
                    .map_err(db_err)
                    .and_then(sqlite_rows_to_result),
            }?;
            Ok(rows)
        })
        .await
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.timed(sql, async {
            let affected = match &self.pool {
                Pool::Pg(p) => {
                    let mut q = sqlx::query(sql);
                    for param in params {
                        q = match param {
                            serde_json::Value::String(s) => q.bind(s.as_str()),
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    q.bind(i)
                                } else if let Some(f) = n.as_f64() {
                                    q.bind(f)
                                } else {
                                    q.bind(n.to_string())
                                }
                            }
                            serde_json::Value::Bool(b) => q.bind(*b),
                            serde_json::Value::Null => q.bind(None::<String>),
                            _ => q.bind(param.to_string()),
                        };
                    }
                    q.execute(p).await.map(|r| r.rows_affected())
                }
                Pool::My(p) => {
                    let mut q = sqlx::query(sql);
                    for param in params {
                        q = match param {
                            serde_json::Value::String(s) => q.bind(s.as_str()),
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    q.bind(i)
                                } else if let Some(f) = n.as_f64() {
                                    q.bind(f)
                                } else {
                                    q.bind(n.to_string())
                                }
                            }
                            serde_json::Value::Bool(b) => q.bind(*b),
                            serde_json::Value::Null => q.bind(None::<String>),
                            _ => q.bind(param.to_string()),
                        };
                    }
                    q.execute(p).await.map(|r| r.rows_affected())
                }
                Pool::Sq(p) => {
                    let mut q = sqlx::query(sql);
                    for param in params {
                        q = match param {
                            serde_json::Value::String(s) => q.bind(s.as_str()),
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    q.bind(i)
                                } else if let Some(f) = n.as_f64() {
                                    q.bind(f)
                                } else {
                                    q.bind(n.to_string())
                                }
                            }
                            serde_json::Value::Bool(b) => q.bind(*b),
                            serde_json::Value::Null => q.bind(None::<String>),
                            _ => q.bind(param.to_string()),
                        };
                    }
                    q.execute(p).await.map(|r| r.rows_affected())
                }
            }
            .map_err(db_err)?;
            Ok(affected)
        })
        .await
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.timed(sql, async {
            let rows = match &self.pool {
                Pool::Pg(p) => {
                    let mut q = sqlx::query(sql);
                    for param in params {
                        q = match param {
                            serde_json::Value::String(s) => q.bind(s.as_str()),
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    q.bind(i)
                                } else if let Some(f) = n.as_f64() {
                                    q.bind(f)
                                } else {
                                    q.bind(n.to_string())
                                }
                            }
                            serde_json::Value::Bool(b) => q.bind(*b),
                            serde_json::Value::Null => q.bind(None::<String>),
                            _ => q.bind(param.to_string()),
                        };
                    }
                    q.fetch_all(p)
                        .await
                        .map_err(db_err)
                        .and_then(pg_rows_to_result)
                }
                Pool::My(p) => {
                    let mut q = sqlx::query(sql);
                    for param in params {
                        q = match param {
                            serde_json::Value::String(s) => q.bind(s.as_str()),
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    q.bind(i)
                                } else if let Some(f) = n.as_f64() {
                                    q.bind(f)
                                } else {
                                    q.bind(n.to_string())
                                }
                            }
                            serde_json::Value::Bool(b) => q.bind(*b),
                            serde_json::Value::Null => q.bind(None::<String>),
                            _ => q.bind(param.to_string()),
                        };
                    }
                    q.fetch_all(p)
                        .await
                        .map_err(db_err)
                        .and_then(mysql_rows_to_result)
                }
                Pool::Sq(p) => {
                    let mut q = sqlx::query(sql);
                    for param in params {
                        q = match param {
                            serde_json::Value::String(s) => q.bind(s.as_str()),
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    q.bind(i)
                                } else if let Some(f) = n.as_f64() {
                                    q.bind(f)
                                } else {
                                    q.bind(n.to_string())
                                }
                            }
                            serde_json::Value::Bool(b) => q.bind(*b),
                            serde_json::Value::Null => q.bind(None::<String>),
                            _ => q.bind(param.to_string()),
                        };
                    }
                    q.fetch_all(p)
                        .await
                        .map_err(db_err)
                        .and_then(sqlite_rows_to_result)
                }
            }?;
            Ok(rows)
        })
        .await
    }

    /// 两步式原子执行：**自己开一个事务**包住两条语句并提交。
    /// ORM 的 MySQL 主键回填走这里（`LAST_INSERT_ID()` 是连接作用域的，
    /// 池下直发两条可能落到不同连接）；一步式后端用不到（默认实现报错）。
    ///
    /// 第一条失败时不提交，`tx` 在 drop 时由底层 sqlx 事务回滚。
    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[serde_json::Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        let tx = self.transaction().await?;
        tx.execute_with(first, first_params).await?;
        let rows = tx.query(second).await?;
        tx.commit().await?;
        Ok(rows)
    }

    fn dialect(&self) -> Dialect {
        // 注意是 `self.pool.dialect()`，写成 `self.dialect()` 会无限递归。
        self.pool.dialect()
    }
}

#[async_trait]
impl RdbmsClient for SqlxClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        let dialect = self.pool.dialect();
        // 超时值随事务一起带走：`transaction()` 之后调用方手里只有 `Transaction`，
        // 事务 wrapper 是唯一知道该用什么超时的地方。
        let query_timeout = self.query_timeout;
        let inner: Box<dyn TransactionInner> = match &self.pool {
            Pool::Pg(p) => Box::new(PgTx {
                inner: Some(p.begin().await.map_err(db_err)?),
                dialect,
                query_timeout,
            }),
            Pool::My(p) => Box::new(MyTx {
                inner: Some(p.begin().await.map_err(db_err)?),
                dialect,
                query_timeout,
            }),
            Pool::Sq(p) => Box::new(SqTx {
                inner: Some(p.begin().await.map_err(db_err)?),
                dialect,
                query_timeout,
            }),
        };
        Ok(Transaction::with_inner(inner))
    }
}

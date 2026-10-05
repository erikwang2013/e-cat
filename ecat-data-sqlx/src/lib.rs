// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
mod cell;
mod config;
mod pool;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use cell::{mysql_rows_to_result, pg_rows_to_result, sqlite_rows_to_result};
use ecat_data::{
    Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner,
    run_with_timeout,
};
use sqlx::Executor as _;
use std::time::Duration;

pub use config::{PoolParams, SqlxConfig};
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
fn db_err(e: sqlx::Error) -> RdbmsError {
    RdbmsError::Database(e.to_string())
}

/// 已结束（提交或回滚过）的事务再执行 SQL 的错误。
fn tx_finished() -> RdbmsError {
    RdbmsError::Database("transaction already finished".into())
}

pub struct SqlxClient {
    pool: Pool,
    query_timeout: Option<Duration>,
    min_connections: u32,
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
    pub fn from_pool(pool: Pool) -> Self {
        Self {
            pool,
            query_timeout: None,
            min_connections: 0,
        }
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
        run_with_timeout(self.query_timeout, async {
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
        run_with_timeout(self.query_timeout, async {
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
        run_with_timeout(self.query_timeout, async {
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
        run_with_timeout(self.query_timeout, async {
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

    fn dialect(&self) -> Dialect {
        // 注意是 `self.pool.dialect()`，写成 `self.dialect()` 会无限递归。
        self.pool.dialect()
    }
}

#[async_trait]
impl RdbmsClient for SqlxClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        let dialect = self.pool.dialect();
        let inner: Box<dyn TransactionInner> = match &self.pool {
            Pool::Pg(p) => Box::new(PgTx {
                inner: Some(p.begin().await.map_err(db_err)?),
                dialect,
            }),
            Pool::My(p) => Box::new(MyTx {
                inner: Some(p.begin().await.map_err(db_err)?),
                dialect,
            }),
            Pool::Sq(p) => Box::new(SqTx {
                inner: Some(p.begin().await.map_err(db_err)?),
                dialect,
            }),
        };
        Ok(Transaction::with_inner(inner))
    }
}

/// 事务 wrapper：三种驱动的 `Transaction<'static, DB>` 是三种类型，
/// 用宏生成三份同构实现，各自配对应的 `*_rows_to_result`。
macro_rules! tx_wrapper {
    ($name:ident, $db:ty, $rows:ident) => {
        struct $name {
            inner: Option<sqlx::Transaction<'static, $db>>,
            dialect: Dialect,
        }

        #[async_trait]
        impl TransactionInner for $name {
            async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError> {
                let tx = self.inner.as_mut().ok_or_else(tx_finished)?;
                tx.execute(sql)
                    .await
                    .map(|r| r.rows_affected())
                    .map_err(db_err)
            }

            async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
                let tx = self.inner.as_mut().ok_or_else(tx_finished)?;
                let rows = tx.fetch_all(sql).await.map_err(db_err)?;
                $rows(rows)
            }

            async fn execute_with(
                &mut self,
                sql: &str,
                params: &[serde_json::Value],
            ) -> Result<u64, RdbmsError> {
                let tx = self.inner.as_mut().ok_or_else(tx_finished)?;
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
                q.execute(&mut **tx)
                    .await
                    .map(|r| r.rows_affected())
                    .map_err(db_err)
            }

            async fn query_with(
                &mut self,
                sql: &str,
                params: &[serde_json::Value],
            ) -> Result<Vec<Row>, RdbmsError> {
                let tx = self.inner.as_mut().ok_or_else(tx_finished)?;
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
                let rows = q.fetch_all(&mut **tx).await.map_err(db_err)?;
                $rows(rows)
            }

            fn dialect(&self) -> Dialect {
                self.dialect
            }

            async fn commit(&mut self) -> Result<(), RdbmsError> {
                if let Some(tx) = self.inner.take() {
                    tx.commit().await.map_err(db_err)?;
                }
                Ok(())
            }

            async fn rollback(&mut self) -> Result<(), RdbmsError> {
                if let Some(tx) = self.inner.take() {
                    tx.rollback().await.map_err(db_err)?;
                }
                Ok(())
            }
        }
    };
}

tx_wrapper!(PgTx, sqlx::Postgres, pg_rows_to_result);
tx_wrapper!(MyTx, sqlx::MySql, mysql_rows_to_result);
tx_wrapper!(SqTx, sqlx::Sqlite, sqlite_rows_to_result);

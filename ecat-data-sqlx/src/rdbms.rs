// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! [`RdbmsClient`] 实现：事务开启、副本延迟探测与连接池大小。
//!
//! 单独成模块是为了守住「每个源文件 < 500 行」的项目硬规则：`lib.rs`
//! 已贴着上限，`SqlExecutor` 实现继续留在那里。
use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, SqlExecutor, Transaction, TransactionInner};
use std::time::Duration;

use crate::pool::Pool;
use crate::transaction::{MyTx, PgTx, SqTx};
use crate::{SqlxClient, db_err, mysql_replica_status_sql, parse_mysql_replica_lag};

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

    /// 只对 MySQL 探测（其余方言返回 `None`）：版本决定用哪条 `SHOW` 语句。
    async fn replica_lag(&self) -> Option<Duration> {
        if self.pool.dialect() != Dialect::MySql {
            return None;
        }
        let version = self.mysql_version().await?;
        let rows = self.query(mysql_replica_status_sql(&version)).await.ok()?;
        parse_mysql_replica_lag(&rows)
    }

    /// 池内当前已建立的连接数（sqlx 的 `Pool::size()`）。
    fn pool_size(&self) -> Option<u32> {
        Some(self.pool.size())
    }
}

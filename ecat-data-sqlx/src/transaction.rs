// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! 事务 wrapper：三种驱动的 `Transaction<'static, DB>` 是三种类型，
//! 用宏生成三份同构实现，各自配对应的 `*_rows_to_result`。
//!
//! 独立成文件是因为 `lib.rs` 有 500 行上限（批次 1 计划「已知的 500 行超限」：
//! 若 `lib.rs` 仍超，把事务 wrapper 移到 `src/transaction.rs`）。

use crate::cell::{mysql_rows_to_result, pg_rows_to_result, sqlite_rows_to_result};
use crate::db_err;
use async_trait::async_trait;
use ecat_data::{BackendKind, Dialect, RdbmsError, Row, TransactionInner, run_with_timeout};
use sqlx::Executor as _;
use std::time::Duration;

/// 已结束（提交或回滚过）的事务再执行 SQL 的错误。
fn tx_finished() -> RdbmsError {
    RdbmsError::Database("transaction already finished".into())
}

/// **超时语义**：四个执行方法与客户端一样套 [`run_with_timeout`] —— 事务里挂死的
/// 查询一样会永久占住连接，事务不是例外。但超时把查询从半路切断后，**事务状态
/// 不再确定**（服务端可能已执行、可能还在执行、也可能已中止），本 wrapper 不会
/// 自作主张回滚，只返回 [`RdbmsError::Timeout`] 交由调用方判断；**调用方应当回滚
/// 或直接丢弃本事务，不要在这个事务上继续执行**。
macro_rules! tx_wrapper {
    ($name:ident, $db:ty, $rows:ident) => {
        pub(crate) struct $name {
            pub(crate) inner: Option<sqlx::Transaction<'static, $db>>,
            pub(crate) dialect: Dialect,
            pub(crate) query_timeout: Option<Duration>,
        }

        #[async_trait]
        impl TransactionInner for $name {
            async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError> {
                run_with_timeout(BackendKind::Rdbms, self.query_timeout, async {
                    let tx = self.inner.as_mut().ok_or_else(tx_finished)?;
                    tx.execute(sql)
                        .await
                        .map(|r| r.rows_affected())
                        .map_err(db_err)
                })
                .await
            }

            async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
                run_with_timeout(BackendKind::Rdbms, self.query_timeout, async {
                    let tx = self.inner.as_mut().ok_or_else(tx_finished)?;
                    let rows = tx.fetch_all(sql).await.map_err(db_err)?;
                    $rows(rows)
                })
                .await
            }

            async fn execute_with(
                &mut self,
                sql: &str,
                params: &[serde_json::Value],
            ) -> Result<u64, RdbmsError> {
                run_with_timeout(BackendKind::Rdbms, self.query_timeout, async {
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
                })
                .await
            }

            async fn query_with(
                &mut self,
                sql: &str,
                params: &[serde_json::Value],
            ) -> Result<Vec<Row>, RdbmsError> {
                run_with_timeout(BackendKind::Rdbms, self.query_timeout, async {
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
                })
                .await
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

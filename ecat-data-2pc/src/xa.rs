// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! sqlx XA 参与者：整个 XA 生命周期固定在**同一条物理连接**上。
//!
//! `XA START/END/PREPARE/EXECUTE` 与 `LAST_INSERT_ID()` 同类，必须落在同一连接；
//! 这里用 `Mutex<HashMap<xid, Pinned>>` 保存连接 guard，绝不能走池化
//! `execute_with`/`query_with`。
use std::collections::HashMap;

use async_trait::async_trait;
use ecat_data::Dialect;
use ecat_data_sqlx::{Pool, PoolGuard, SqlxClient};
use serde_json::Value;
use sqlx::Row as _;
use tokio::sync::Mutex;

use crate::coordinator::{XaError, XaParticipant};

struct Pinned {
    guard: PoolGuard,
    prepared: bool,
}

/// MySQL/PostgreSQL XA 参与者；其他方言构造时直接拒绝。
pub struct SqlxXaParticipant {
    dialect: Dialect,
    pool: Pool,
    conns: Mutex<HashMap<String, Pinned>>,
}

impl SqlxXaParticipant {
    pub fn new(client: &SqlxClient) -> Result<Self, XaError> {
        let dialect = client.pool().dialect();
        if !matches!(dialect, Dialect::MySql | Dialect::Postgres) {
            return Err(XaError::UnsupportedDialect);
        }
        Ok(Self {
            dialect,
            pool: client.pool().clone(),
            conns: Mutex::new(HashMap::new()),
        })
    }

    async fn acquire(&self) -> Result<PoolGuard, XaError> {
        self.pool.acquire().await.map_err(db_error)
    }
}

#[async_trait]
impl XaParticipant for SqlxXaParticipant {
    async fn xa_start(&self, xid: &str) -> Result<(), XaError> {
        // 不在持有 conns 锁时等连接：池满时 commit 需要拿回连接并回收 guard，
        // 持锁等池会与它互等。
        if self.conns.lock().await.contains_key(xid) {
            return Ok(());
        }
        let mut guard = self.acquire().await?;
        let sql = match self.dialect {
            Dialect::MySql => format!("XA START '{}'", quote_xid(xid)),
            Dialect::Postgres => "BEGIN".to_owned(),
            _ => return Err(XaError::UnsupportedDialect),
        };
        exec_guard(&mut guard, &sql).await?;
        let mut conns = self.conns.lock().await;
        if conns.contains_key(xid) {
            // 另一个调用已为同一 xid 固定了连接；这条新建的活动事务必须回滚。
            let rollback = match self.dialect {
                Dialect::MySql => format!("XA ROLLBACK '{}'", quote_xid(xid)),
                Dialect::Postgres => "ROLLBACK".to_owned(),
                _ => return Err(XaError::UnsupportedDialect),
            };
            let _ = exec_guard(&mut guard, &rollback).await;
            return Err(XaError::UnknownXid(xid.to_owned()));
        }
        conns.insert(
            xid.to_owned(),
            Pinned {
                guard,
                prepared: false,
            },
        );
        Ok(())
    }

    async fn xa_end(&self, xid: &str) -> Result<(), XaError> {
        let mut conns = self.conns.lock().await;
        let pinned = conns
            .get_mut(xid)
            .ok_or_else(|| XaError::UnknownXid(xid.to_owned()))?;
        match self.dialect {
            Dialect::MySql => {
                exec_guard(&mut pinned.guard, &format!("XA END '{}'", quote_xid(xid)))
                    .await
                    .map(|_| ())
            }
            Dialect::Postgres => Ok(()),
            _ => Err(XaError::UnsupportedDialect),
        }
    }

    async fn xa_prepare(&self, xid: &str) -> Result<(), XaError> {
        let mut conns = self.conns.lock().await;
        let pinned = conns
            .get_mut(xid)
            .ok_or_else(|| XaError::UnknownXid(xid.to_owned()))?;
        let sql = match self.dialect {
            Dialect::MySql => format!("XA PREPARE '{}'", quote_xid(xid)),
            Dialect::Postgres => format!("PREPARE TRANSACTION '{}'", quote_xid(xid)),
            _ => return Err(XaError::UnsupportedDialect),
        };
        exec_guard(&mut pinned.guard, &sql).await?;
        pinned.prepared = true;
        Ok(())
    }

    async fn xa_commit(&self, xid: &str) -> Result<(), XaError> {
        // 先取走 guard 并释放锁；池满时等待不能挡住同一参与者的其它命令。
        let pinned = self.conns.lock().await.remove(xid);
        let mut guard = match pinned {
            Some(pinned) => pinned.guard,
            None => self.acquire().await?,
        };
        let sql = match self.dialect {
            Dialect::MySql => format!("XA COMMIT '{}'", quote_xid(xid)),
            Dialect::Postgres => format!("COMMIT PREPARED '{}'", quote_xid(xid)),
            _ => return Err(XaError::UnsupportedDialect),
        };
        match exec_guard(&mut guard, &sql).await {
            Ok(_) => Ok(()),
            Err(error) if unknown_xid(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn xa_rollback(&self, xid: &str) -> Result<(), XaError> {
        let pinned = self.conns.lock().await.remove(xid);
        let (mut guard, active) = match pinned {
            Some(pinned) => (pinned.guard, !pinned.prepared),
            None => (self.acquire().await?, false),
        };
        let sql = match self.dialect {
            Dialect::MySql => format!("XA ROLLBACK '{}'", quote_xid(xid)),
            Dialect::Postgres if active => "ROLLBACK".to_owned(),
            Dialect::Postgres => format!("ROLLBACK PREPARED '{}'", quote_xid(xid)),
            _ => return Err(XaError::UnsupportedDialect),
        };
        match exec_guard(&mut guard, &sql).await {
            Ok(_) => Ok(()),
            Err(error) if unknown_xid(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn xa_recover(&self) -> Result<Vec<String>, XaError> {
        let mut guard = self.acquire().await?;
        match &mut guard {
            PoolGuard::My(conn) => {
                let rows = sqlx::query("XA RECOVER")
                    .fetch_all(&mut **conn)
                    .await
                    .map_err(db_error)?;
                rows.iter()
                    .map(|row| {
                        let data: String = row.try_get("data").map_err(db_error)?;
                        Ok(mysql_xid(&data))
                    })
                    .collect()
            }
            PoolGuard::Pg(conn) => {
                let rows = sqlx::query("SELECT gid FROM pg_prepared_xacts")
                    .fetch_all(&mut **conn)
                    .await
                    .map_err(db_error)?;
                rows.iter()
                    .map(|row| {
                        let gid: String = row.try_get("gid").map_err(db_error)?;
                        Ok(gid)
                    })
                    .collect()
            }
            PoolGuard::Sq(_) => Err(XaError::UnsupportedDialect),
        }
    }

    async fn execute(&self, xid: &str, sql: &str, params: &[Value]) -> Result<u64, XaError> {
        let mut conns = self.conns.lock().await;
        let pinned = conns
            .get_mut(xid)
            .ok_or_else(|| XaError::UnknownXid(xid.to_owned()))?;
        exec_params(&mut pinned.guard, sql, params).await
    }
}

async fn exec_guard(guard: &mut PoolGuard, sql: &str) -> Result<u64, XaError> {
    match guard {
        PoolGuard::My(conn) => sqlx::query(sql)
            .execute(&mut **conn)
            .await
            .map(|result| result.rows_affected())
            .map_err(db_error),
        PoolGuard::Pg(conn) => sqlx::query(sql)
            .execute(&mut **conn)
            .await
            .map(|result| result.rows_affected())
            .map_err(db_error),
        PoolGuard::Sq(_) => Err(XaError::UnsupportedDialect),
    }
}

async fn exec_params<'q>(
    guard: &mut PoolGuard,
    sql: &'q str,
    params: &'q [Value],
) -> Result<u64, XaError> {
    match guard {
        PoolGuard::My(conn) => {
            let mut query = sqlx::query(sql);
            for value in params {
                query = bind_mysql(query, value)?;
            }
            query
                .execute(&mut **conn)
                .await
                .map(|result| result.rows_affected())
                .map_err(db_error)
        }
        PoolGuard::Pg(conn) => {
            let mut query = sqlx::query(sql);
            for value in params {
                query = bind_postgres(query, value)?;
            }
            query
                .execute(&mut **conn)
                .await
                .map(|result| result.rows_affected())
                .map_err(db_error)
        }
        PoolGuard::Sq(_) => Err(XaError::UnsupportedDialect),
    }
}

type MysqlQuery<'q> = sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>;
type PostgresQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

fn bind_mysql<'q>(query: MysqlQuery<'q>, value: &'q Value) -> Result<MysqlQuery<'q>, XaError> {
    Ok(match value {
        Value::String(text) => query.bind(text.as_str()),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                query.bind(value)
            } else if let Some(value) = number.as_u64() {
                query.bind(value)
            } else if let Some(value) = number.as_f64() {
                query.bind(value)
            } else {
                return Err(XaError::Database("unsupported XA number".into()));
            }
        }
        Value::Bool(flag) => query.bind(*flag),
        Value::Null => query.bind(None::<String>),
        other => {
            return Err(XaError::Database(format!(
                "unsupported XA parameter type: {other}"
            )));
        }
    })
}

fn bind_postgres<'q>(
    query: PostgresQuery<'q>,
    value: &'q Value,
) -> Result<PostgresQuery<'q>, XaError> {
    Ok(match value {
        Value::String(text) => query.bind(text.as_str()),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                query.bind(value)
            } else if let Some(value) = number.as_f64() {
                query.bind(value)
            } else {
                return Err(XaError::Database(
                    "u64 XA parameters are unsupported on PostgreSQL".into(),
                ));
            }
        }
        Value::Bool(flag) => query.bind(*flag),
        Value::Null => query.bind(None::<String>),
        other => {
            return Err(XaError::Database(format!(
                "unsupported XA parameter type: {other}"
            )));
        }
    })
}

fn quote_xid(xid: &str) -> String {
    xid.replace('\'', "''")
}

fn mysql_xid(data: &str) -> String {
    // XA RECOVER 的 data 形如 `gtrid,bqual,formatID`；UUID 不含逗号。
    data.rsplit_once(',')
        .map_or(data, |(gtrid, _)| gtrid)
        .to_owned()
}

fn unknown_xid(error: &XaError) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("xaer_nota") || text.contains("does not exist") || text.contains("unknown xid")
}

fn db_error(error: sqlx::Error) -> XaError {
    XaError::Database(error.to_string())
}

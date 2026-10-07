// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use crate::config::PoolParams;
use ecat_data::Dialect;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

/// 三种原生池。方言由变体本身承载 —— 不再解析 URL 猜测。
///
/// `Clone`：sqlx 的池内部是 `Arc`，克隆只是多一个句柄（`metrics` feature 的
/// collector 要长期持有池）。
#[derive(Debug, Clone)]
pub enum Pool {
    Pg(PgPool),
    My(MySqlPool),
    Sq(SqlitePool),
}

/// 三种驱动的连接 guard 的聚合类型。Drop 即归还连接。
/// `PoolConnection<DB>` 自身持有 `Arc`，无需生命周期参数。
pub enum PoolGuard {
    Pg(sqlx::pool::PoolConnection<sqlx::Postgres>),
    My(sqlx::pool::PoolConnection<sqlx::MySql>),
    Sq(sqlx::pool::PoolConnection<sqlx::Sqlite>),
}

impl Pool {
    /// 按 URL scheme 建立对应的原生池。
    pub async fn connect(url: &str, params: &PoolParams) -> Result<Self, sqlx::Error> {
        match Dialect::from_url(url) {
            Dialect::Postgres => Ok(Self::Pg(
                common_pg(PgPoolOptions::new(), params).connect(url).await?,
            )),
            Dialect::MySql => Ok(Self::My(
                common_mysql(MySqlPoolOptions::new(), params)
                    .connect(url)
                    .await?,
            )),
            Dialect::Sqlite => Ok(Self::Sq(
                common_sqlite(SqlitePoolOptions::new(), params)
                    .connect(url)
                    .await?,
            )),
            // 报 scheme 原文而非 Dialect：`Dialect::Standard` 是「无法识别」的
            // 回退值，把它打进错误信息等于丢掉唯一的诊断线索。
            Dialect::Standard => Err(unsupported(url)),
            // Mssql 走 tiberius 后端（ecat-data-mssql），不是 sqlx
            Dialect::Mssql => Err(sqlx::Error::Configuration(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "mssql:// is served by ecat-data-mssql, not the sqlx backend",
            )))),
        }
    }

    pub fn dialect(&self) -> Dialect {
        match self {
            Self::Pg(_) => Dialect::Postgres,
            Self::My(_) => Dialect::MySql,
            Self::Sq(_) => Dialect::Sqlite,
        }
    }

    /// 池内已建立的连接总数。
    pub fn size(&self) -> u32 {
        match self {
            Self::Pg(p) => p.size(),
            Self::My(p) => p.size(),
            Self::Sq(p) => p.size(),
        }
    }

    /// 池内空闲连接数。
    pub fn idle(&self) -> u32 {
        match self {
            Self::Pg(p) => p.num_idle() as u32,
            Self::My(p) => p.num_idle() as u32,
            Self::Sq(p) => p.num_idle() as u32,
        }
    }

    /// 从池中取一条连接（预热用）。归还由 guard 的 Drop 完成。
    pub async fn acquire(&self) -> Result<PoolGuard, sqlx::Error> {
        Ok(match self {
            Self::Pg(p) => PoolGuard::Pg(p.acquire().await?),
            Self::My(p) => PoolGuard::My(p.acquire().await?),
            Self::Sq(p) => PoolGuard::Sq(p.acquire().await?),
        })
    }
}

/// 无法识别的 scheme：错误信息要点名出问题的 scheme。
fn unsupported(url: &str) -> sqlx::Error {
    let scheme = url
        .split("://")
        .next()
        .unwrap_or(url)
        .split(':')
        .next()
        .unwrap_or(url);
    sqlx::Error::Configuration(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("unsupported database scheme for sqlx backend: {scheme}"),
    )))
}

// 三个 common_* 把 PoolParams 落到各驱动的 PoolOptions 上。
// 注意：三者的 Connection 类型不同，after_connect 闭包无法合并，各写一份。

fn common_pg(opts: PgPoolOptions, p: &PoolParams) -> PgPoolOptions {
    let opts = opts
        .max_connections(p.max_connections)
        .min_connections(p.min_connections)
        .acquire_timeout(p.acquire_timeout)
        .idle_timeout(Some(p.idle_timeout))
        .max_lifetime(Some(p.max_lifetime))
        .test_before_acquire(p.test_before_acquire);
    if p.session_init.is_empty() {
        return opts;
    }
    let stmts = p.session_init.clone();
    opts.after_connect(move |conn, _meta| {
        let stmts = stmts.clone();
        Box::pin(async move {
            for s in &stmts {
                sqlx::query(s).execute(&mut *conn).await?;
            }
            Ok(())
        })
    })
}

fn common_mysql(opts: MySqlPoolOptions, p: &PoolParams) -> MySqlPoolOptions {
    let opts = opts
        .max_connections(p.max_connections)
        .min_connections(p.min_connections)
        .acquire_timeout(p.acquire_timeout)
        .idle_timeout(Some(p.idle_timeout))
        .max_lifetime(Some(p.max_lifetime))
        .test_before_acquire(p.test_before_acquire);
    if p.session_init.is_empty() {
        return opts;
    }
    let stmts = p.session_init.clone();
    opts.after_connect(move |conn, _meta| {
        let stmts = stmts.clone();
        Box::pin(async move {
            for s in &stmts {
                sqlx::query(s).execute(&mut *conn).await?;
            }
            Ok(())
        })
    })
}

fn common_sqlite(opts: SqlitePoolOptions, p: &PoolParams) -> SqlitePoolOptions {
    let opts = opts
        .max_connections(p.max_connections)
        .min_connections(p.min_connections)
        .acquire_timeout(p.acquire_timeout)
        .idle_timeout(Some(p.idle_timeout))
        .max_lifetime(Some(p.max_lifetime))
        .test_before_acquire(p.test_before_acquire);
    if p.session_init.is_empty() {
        return opts;
    }
    let stmts = p.session_init.clone();
    opts.after_connect(move |conn, _meta| {
        let stmts = stmts.clone();
        Box::pin(async move {
            for s in &stmts {
                sqlx::query(s).execute(&mut *conn).await?;
            }
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_data::Dialect;

    #[tokio::test]
    async fn sqlite_url_builds_sqlite_pool() {
        let pool = Pool::connect("sqlite::memory:", &PoolParams::for_url("sqlite::memory:"))
            .await
            .unwrap();
        assert_eq!(pool.dialect(), Dialect::Sqlite);
        assert!(matches!(pool, Pool::Sq(_)));
    }

    /// 无法识别的 scheme 必须被拒，且**错误信息要点名出问题的 scheme**。
    #[test]
    fn unsupported_scheme_is_rejected_and_names_the_scheme() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(Pool::connect("oracle://h/db", &PoolParams::default()))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unsupported"), "got: {msg}");
        assert!(
            msg.contains("oracle"),
            "错误信息必须点名 scheme，got: {msg}"
        );
    }

    /// Mssql 归 ecat-data-mssql（tiberius），不应被 sqlx 后端静默接受。
    #[test]
    fn mssql_scheme_is_rejected_by_sqlx_backend() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(Pool::connect("mssql://h:1433/db", &PoolParams::default()))
            .unwrap_err();
        assert!(err.to_string().contains("ecat-data-mssql"), "got: {err}");
    }
}

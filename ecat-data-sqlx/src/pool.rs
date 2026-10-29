// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use crate::config::{MysqlCompat, PoolParams};
use ecat_data::Dialect;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::str::FromStr;

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
        Self::connect_with_mysql_compat(url, params, MysqlCompat::Mysql).await
    }

    /// 同 [`Pool::connect`]，但可指定 MySQL 协议兼容模式。
    pub async fn connect_with_mysql_compat(
        url: &str,
        params: &PoolParams,
        compat: MysqlCompat,
    ) -> Result<Self, sqlx::Error> {
        match Dialect::from_url(url) {
            Dialect::Postgres => Ok(Self::Pg(
                common_pg(PgPoolOptions::new(), params).connect(url).await?,
            )),
            Dialect::MySql => Ok(Self::My(
                common_mysql(MySqlPoolOptions::new(), params)
                    .connect_with(mysql_options(url, compat)?)
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

    /// 池配置的最大连接数（预算校验用）。注意 [`Pool::size`] 是当前已建立数，
    /// 空闲池的 `size()` 不是上限。
    pub fn max_connections(&self) -> u32 {
        match self {
            Self::Pg(p) => p.options().get_max_connections(),
            Self::My(p) => p.options().get_max_connections(),
            Self::Sq(p) => p.options().get_max_connections(),
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

/// 解析 MySQL URL，并按兼容模式关闭 sqlx 默认的 sql_mode 子查询。
///
/// StarRocks 不支持 `SET sql_mode=(SELECT CONCAT(...))`（sqlx issue #3205），
/// 而 `pipes_as_concat` / `no_engine_substitution` 只能通过连接选项设置，URL
/// 参数无法表达。Doris / StarRocks 同时显式使用 utf8mb4 通用排序规则，避免
/// sqlx 默认的 `utf8mb4_0900_ai_ci` 在旧服务端不存在。
fn mysql_options(url: &str, compat: MysqlCompat) -> Result<MySqlConnectOptions, sqlx::Error> {
    let opts = MySqlConnectOptions::from_str(url)?;
    if compat == MysqlCompat::Mysql {
        return Ok(opts);
    }
    Ok(opts
        .pipes_as_concat(false)
        .no_engine_substitution(false)
        .charset("utf8mb4")
        .collation("utf8mb4_general_ci"))
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

    /// Doris / StarRocks 兼容模式必须关掉 sqlx 默认的 sql_mode 子查询；
    /// `MySqlConnectOptions` 的字段是私有的，这里用 Debug 输出钉住三个开关。
    #[test]
    fn starrocks_compat_disables_sql_mode_subquery() {
        let opts = mysql_options("mysql://h:9030/db", MysqlCompat::StarRocks).unwrap();
        let debug = format!("{opts:?}");
        assert!(debug.contains("pipes_as_concat: false"), "{debug}");
        assert!(debug.contains("no_engine_substitution: false"), "{debug}");
        assert!(
            debug.contains("collation: Some(\"utf8mb4_general_ci\")"),
            "{debug}"
        );
    }

    #[test]
    fn mysql_default_keeps_sqlx_session_settings() {
        let opts = mysql_options("mysql://h/db", MysqlCompat::Mysql).unwrap();
        let debug = format!("{opts:?}");
        assert!(debug.contains("pipes_as_concat: true"), "{debug}");
        assert!(debug.contains("no_engine_substitution: true"), "{debug}");
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

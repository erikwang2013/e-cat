// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

/// 数据库方言标识。
///
/// 放在 `ecat-data` 而非 `ecat-orm`：`ecat-data-sqlx` 需要上报自己的方言，
/// 而它不能依赖 `ecat-orm`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// ANSI 近似（双引号标识符、`?` 占位符、`LIMIT`）。
    /// 第三方 `SqlExecutor` 实现未声明方言时的回退值。
    Standard,
    Sqlite,
    Postgres,
    MySql,
    Mssql,
}

impl Dialect {
    /// 从连接串推断方言，无法识别时返回 [`Dialect::Standard`]。
    ///
    /// 同时兼容有 `://` 的形式（`postgres://host/db`）与 sqlite 的无 authority
    /// 形式（`sqlite:app.db`）。
    pub fn from_url(url: &str) -> Self {
        // RFC 3986 §3.1：scheme 大小写不敏感。sqlx 用 url crate 解析连接串时会
        // 小写化 scheme，因此 "POSTGRES://host/db" 能连通 —— 这里不归一化就会
        // 静默返回 Standard，与后端实际行为不一致。
        let scheme = url
            .split("://")
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match scheme.as_str() {
            "postgres" | "postgresql" => Self::Postgres,
            "mysql" | "mariadb" => Self::MySql,
            "sqlite" => Self::Sqlite,
            "mssql" | "sqlserver" => Self::Mssql,
            _ => Self::Standard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_url_recognizes_each_scheme() {
        assert_eq!(
            Dialect::from_url("postgres://localhost/db"),
            Dialect::Postgres
        );
        assert_eq!(
            Dialect::from_url("postgresql://localhost/db"),
            Dialect::Postgres
        );
        assert_eq!(Dialect::from_url("mysql://localhost/db"), Dialect::MySql);
        assert_eq!(Dialect::from_url("mariadb://localhost/db"), Dialect::MySql);
        assert_eq!(Dialect::from_url("sqlite::memory:"), Dialect::Sqlite);
        assert_eq!(Dialect::from_url("sqlite:app.db"), Dialect::Sqlite);
        assert_eq!(Dialect::from_url("mssql://host:1433/db"), Dialect::Mssql);
        assert_eq!(
            Dialect::from_url("sqlserver://host:1433/db"),
            Dialect::Mssql
        );
        assert_eq!(Dialect::from_url("postgres"), Dialect::Postgres);
    }

    #[test]
    fn from_url_unknown_scheme_is_standard() {
        assert_eq!(Dialect::from_url("oracle://host/db"), Dialect::Standard);
        assert_eq!(Dialect::from_url(""), Dialect::Standard);
        assert_eq!(Dialect::from_url("nonsense"), Dialect::Standard);
    }

    /// RFC 3986 §3.1：scheme 大小写不敏感。sqlx 侧同样会小写化，
    /// 不归一化就会在能连通的情况下静默给出 Standard。
    #[test]
    fn from_url_is_case_insensitive() {
        assert_eq!(
            Dialect::from_url("POSTGRES://localhost/db"),
            Dialect::Postgres
        );
        assert_eq!(Dialect::from_url("MySQL://localhost/db"), Dialect::MySql);
        assert_eq!(Dialect::from_url("SQLite:app.db"), Dialect::Sqlite);
    }

    /// sqlite 的 URL 没有 `://`，是最容易写错的一类，单独钉住。
    #[test]
    fn from_url_handles_sqlite_without_authority() {
        assert_eq!(
            Dialect::from_url("sqlite:ecat-test.db?mode=memory"),
            Dialect::Sqlite
        );
    }
}

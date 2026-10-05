// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use ecat_data::Dialect;
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct SqlxConfig {
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// SQLx 的 TLS 通过 URL 参数配置（如 `?sslmode=require`）。
    /// 本字段预留给未来的程序化 TLS 支持。
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 会话初始化语句；未配置时按方言给默认值，见
    /// [`SqlxConfig::effective_session_init`]。
    #[serde(default)]
    pub session_init: Option<Vec<String>>,

    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub min_connections: Option<u32>,
    #[serde(default)]
    pub acquire_timeout_secs: Option<u64>,
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_lifetime_secs: Option<u64>,
    /// `0` = 禁用查询超时。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 每次从池取连接是否先 ping。默认 `false`：sqlx 默认在**每次** acquire
    /// 都 ping（不论上次 ping 多近，见 sqlx issue #1743），在高频路径上是
    /// 一笔无谓往返。关掉后由 `max_lifetime` + 查询超时 + 首次使用报错兜底。
    #[serde(default)]
    pub test_before_acquire: Option<bool>,
}

/// 池参数的解析结果（带默认值）。由 [`SqlxConfig::pool`] 产出。
#[derive(Debug, Clone)]
pub struct PoolParams {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    pub query_timeout: Option<Duration>,
    pub test_before_acquire: bool,
    /// 每条新连接建立后执行的会话初始化语句。
    pub session_init: Vec<String>,
}

impl Default for PoolParams {
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
            max_lifetime: Duration::from_secs(1800),
            query_timeout: Some(Duration::from_secs(30)),
            test_before_acquire: false,
            session_init: Vec::new(),
        }
    }
}

impl SqlxConfig {
    pub fn pool(&self) -> PoolParams {
        PoolParams {
            max_connections: self.max_connections.unwrap_or(10),
            min_connections: self.min_connections.unwrap_or(0),
            acquire_timeout: Duration::from_secs(self.acquire_timeout_secs.unwrap_or(30)),
            idle_timeout: Duration::from_secs(self.idle_timeout_secs.unwrap_or(600)),
            max_lifetime: Duration::from_secs(self.max_lifetime_secs.unwrap_or(1800)),
            query_timeout: self.query_timeout(),
            test_before_acquire: self.test_before_acquire.unwrap_or(false),
            session_init: self.effective_session_init(),
        }
    }

    /// `0` 表示显式禁用超时；未配置时为 30 秒。
    pub fn query_timeout(&self) -> Option<Duration> {
        match self.query_timeout_secs {
            None => Some(Duration::from_secs(30)),
            Some(0) => None,
            Some(s) => Some(Duration::from_secs(s)),
        }
    }

    /// 未显式配置时按方言给默认会话初始化语句。
    /// SQLite 无会话概念，返回空。
    pub fn effective_session_init(&self) -> Vec<String> {
        if let Some(custom) = &self.session_init {
            return custom.clone();
        }
        match Dialect::from_url(&self.url) {
            Dialect::Postgres => vec![
                "SET TIME ZONE 'UTC'".to_string(),
                "SET application_name = 'ecat'".to_string(),
            ],
            Dialect::MySql => vec!["SET time_zone = '+00:00'".to_string()],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_uses_documented_defaults() {
        let cfg: SqlxConfig =
            serde_json::from_str(r#"{"url": "postgres://localhost/db"}"#).unwrap();
        let p = cfg.pool();
        assert_eq!(p.max_connections, 10);
        assert_eq!(p.min_connections, 0);
        assert_eq!(p.acquire_timeout, Duration::from_secs(30));
        assert_eq!(p.idle_timeout, Duration::from_secs(600));
        assert_eq!(p.max_lifetime, Duration::from_secs(1800));
        assert_eq!(p.query_timeout, Some(Duration::from_secs(30)));
        assert!(!p.test_before_acquire);
        assert!(cfg.session_init.is_none());
    }

    #[test]
    fn pool_params_can_be_overridden() {
        let cfg: SqlxConfig = serde_json::from_str(
            r#"{"url": "mysql://localhost/db",
                "max_connections": 32,
                "query_timeout_secs": 0,
                "test_before_acquire": true,
                "session_init": ["SET time_zone = '+00:00'"]}"#,
        )
        .unwrap();
        assert_eq!(cfg.pool().max_connections, 32);
        assert_eq!(cfg.pool().query_timeout, None);
        assert!(cfg.pool().test_before_acquire);
        assert_eq!(
            cfg.effective_session_init(),
            vec!["SET time_zone = '+00:00'".to_string()]
        );
    }

    /// 未显式配置时按方言给默认会话设置：库侧直接返回 UTC，
    /// 与 ORM 的「时间统一 UTC」约定对齐。
    #[test]
    fn session_init_defaults_per_dialect() {
        let pg: SqlxConfig = serde_json::from_str(r#"{"url": "postgres://h/db"}"#).unwrap();
        assert!(
            pg.effective_session_init()
                .iter()
                .any(|s| s.contains("TIME ZONE"))
        );

        let sqlite: SqlxConfig = serde_json::from_str(r#"{"url": "sqlite::memory:"}"#).unwrap();
        assert!(sqlite.effective_session_init().is_empty());
    }

    /// `query_timeout_secs: 0` 是「禁用」而非「0 秒立刻超时」。
    #[test]
    fn zero_query_timeout_means_disabled() {
        let cfg: SqlxConfig =
            serde_json::from_str(r#"{"url": "sqlite::memory:", "query_timeout_secs": 0}"#).unwrap();
        assert_eq!(cfg.query_timeout(), None);
    }
}

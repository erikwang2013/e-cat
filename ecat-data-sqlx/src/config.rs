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
    /// **当前不支持**。SQLx 的 TLS 走 URL 参数（如 `?sslmode=require`）；
    /// 设了本字段会在 [`crate::SqlxClient::from_config`] 处**报错**，
    /// 而不是静默忽略。
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
    /// 结构体默认值：池大小/超时按文档默认，`session_init` 为**空**
    /// （不带任何会话设置）。要「按方言的默认」请用 [`PoolParams::for_url`]。
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

impl PoolParams {
    /// 按 URL 的方言给出「未配置时」的默认参数 —— **含方言默认的 `session_init`**。
    ///
    /// 与 [`PoolParams::default`] 的区别：`Default` 是结构体默认值（无会话初始化），
    /// 本方法才是「用户没配任何东西时应当得到什么」。
    /// `connect()` 与 `from_config()` 都必须走这一条，否则两个构造器行为会静默不一致。
    pub fn for_url(url: &str) -> Self {
        Self {
            session_init: dialect_session_init(url),
            ..Self::default()
        }
    }
}

/// 未显式配置时按方言给默认会话初始化语句（库侧时区设为 UTC，
/// 与 ORM 的「时间统一 UTC」约定对齐）。SQLite 无会话概念，返回空。
pub fn dialect_session_init(url: &str) -> Vec<String> {
    match Dialect::from_url(url) {
        Dialect::Postgres => vec![
            "SET TIME ZONE 'UTC'".to_string(),
            "SET application_name = 'ecat'".to_string(),
        ],
        Dialect::MySql => vec!["SET time_zone = '+00:00'".to_string()],
        _ => Vec::new(),
    }
}

impl SqlxConfig {
    /// 未配置的字段一律取 [`PoolParams::default`] —— 默认值只此一处定义，
    /// 避免与 [`PoolParams::for_url`] 静默分叉。
    pub fn pool(&self) -> PoolParams {
        let d = PoolParams::default();
        let max_connections = self.max_connections.unwrap_or(d.max_connections);
        PoolParams {
            max_connections,
            // min 必须 ≤ max：sqlx 自己的后台保底是「尽力而为」—— 拿不到 permit 就
            // 安静收手（`sqlx-core-0.8.6/src/pool/inner.rs:396` 的 `try_min_connections`），
            // 但 `warm_up()` 是**持着已取的连接**再去 acquire 到 min 条，池上限更低
            // 时永远取不满，只会阻塞到 acquire_timeout 后报错。
            min_connections: self
                .min_connections
                .unwrap_or(d.min_connections)
                .min(max_connections),
            acquire_timeout: self
                .acquire_timeout_secs
                .map_or(d.acquire_timeout, Duration::from_secs),
            idle_timeout: self
                .idle_timeout_secs
                .map_or(d.idle_timeout, Duration::from_secs),
            max_lifetime: self
                .max_lifetime_secs
                .map_or(d.max_lifetime, Duration::from_secs),
            query_timeout: self.query_timeout(),
            test_before_acquire: self.test_before_acquire.unwrap_or(d.test_before_acquire),
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

    /// 会话初始化语句：显式配置优先，未配置时按方言取默认
    /// （见 `dialect_session_init`）。
    ///
    /// 显式空数组会覆盖方言默认，即主动关闭会话初始化
    /// （与 `query_timeout_secs: 0` 表示禁用同族）。
    pub fn effective_session_init(&self) -> Vec<String> {
        self.session_init
            .clone()
            .unwrap_or_else(|| dialect_session_init(&self.url))
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

    /// 未配置时 `for_url`（connect 走它）与 `pool()`（from_config 走它）
    /// 必须逐字段一致 —— 否则两个构造器静默分叉，且不会有任何测试变红。
    #[test]
    fn for_url_matches_pool_defaults_field_by_field() {
        for url in ["postgres://h/db", "mysql://h/db", "sqlite::memory:"] {
            let cfg: SqlxConfig = serde_json::from_str(&format!(r#"{{"url": "{url}"}}"#)).unwrap();
            let a = PoolParams::for_url(url);
            let b = cfg.pool();
            assert_eq!(a.max_connections, b.max_connections, "{url}");
            assert_eq!(a.min_connections, b.min_connections, "{url}");
            assert_eq!(a.acquire_timeout, b.acquire_timeout, "{url}");
            assert_eq!(a.idle_timeout, b.idle_timeout, "{url}");
            assert_eq!(a.max_lifetime, b.max_lifetime, "{url}");
            assert_eq!(a.query_timeout, b.query_timeout, "{url}");
            assert_eq!(a.test_before_acquire, b.test_before_acquire, "{url}");
            assert_eq!(a.session_init, b.session_init, "{url}");
        }
    }

    /// `min > max` 必须夹到 max：否则 `warm_up()` 阻塞到 acquire_timeout 才报错。
    #[test]
    fn min_connections_is_clamped_to_max() {
        let cfg: SqlxConfig = serde_json::from_str(
            r#"{"url": "sqlite::memory:", "max_connections": 2, "min_connections": 5}"#,
        )
        .unwrap();
        let p = cfg.pool();
        assert_eq!(p.max_connections, 2);
        assert_eq!(p.min_connections, 2);
    }

    /// `query_timeout_secs: 0` 是「禁用」而非「0 秒立刻超时」。
    #[test]
    fn zero_query_timeout_means_disabled() {
        let cfg: SqlxConfig =
            serde_json::from_str(r#"{"url": "sqlite::memory:", "query_timeout_secs": 0}"#).unwrap();
        assert_eq!(cfg.query_timeout(), None);
    }

    /// 设了 `tls` 必须响亮失败 —— 曾静默忽略，与「不伪造/不静默」的原则冲突。
    #[tokio::test]
    async fn tls_field_is_rejected_not_silently_ignored() {
        let cfg: SqlxConfig =
            serde_json::from_str(r#"{"url": "postgres://h/db", "tls": {"skip_verify": true}}"#)
                .unwrap();
        assert!(
            cfg.tls.is_some(),
            "serde 应当照旧接受该字段（报错发生在 from_config）"
        );

        // 不用 unwrap_err（SqlxClient 未实现 Debug，为测试给它加 derive 是本末倒置）
        let Err(err) = crate::SqlxClient::from_config(cfg).await else {
            panic!("tls 字段设了必须报错，不能静默忽略");
        };
        let msg = err.to_string();
        assert!(msg.contains("tls"), "got: {msg}");
        assert!(
            msg.contains("sslmode"),
            "错误信息要指向正确做法，got: {msg}"
        );
    }
}

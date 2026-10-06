// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use ecat_data::{Dialect, RdbmsError};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::time::Duration;
// 包名是 `tiberius-ng`，lib 名是 `tiberius`（`Cargo.toml` 的 `[lib] name`）。
use tiberius::{AuthMethod, Config};

/// 连接串里没写端口时的默认端口（默认实例）。
const DEFAULT_PORT: u16 = 1433;

/// SQL Server 连接配置。
///
/// 除 [`Self::url`] 外的字段都是可选的：要么覆盖连接串里的值，要么是池参数。
///
/// 注意：本后端**没有** `idle_timeout` —— deadpool 0.13 不提供空闲回收。
/// 需要空闲回收时用 `max_connections` 限制规模、或依赖查询超时与
/// `recycle_timeout` 兜底。（sqlx 后端有 `idle_timeout_secs`，那边是 sqlx 原生支持。）
///
/// 未知键**响亮失败**（`deny_unknown_fields`）：拼错的键名（`acquire_timeout_second`
/// 少个 s）、或已删掉的 `idle_timeout_secs`，静默走默认值等于「配了却没生效」，
/// 用户还以为生效了。本 crate 是新后端、没有既有配置文件包袱，所以从严；
/// 其它 `XxxConfig` 要不要照办是独立决策，不在本 crate 内替它们定。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MssqlConfig {
    /// 连接串。两种形态都接受：
    /// - URL：`mssql://user:pass@host:1433/db`（`sqlserver://` 同义）
    /// - ADO：`Server=host,1433;Database=db;User Id=user;Password=pass`
    ///
    /// URL 形态**不做 percent-decode**（`%40` 不会被还原成 `@`），密码里要写
    /// `@` / `/` 等分隔符请改用 ADO 形态；也不支持查询串，见 [`Self::from_str`]。
    /// IPv6 字面量写方括号（`mssql://[::1]:1433/db`），方括号会保留 ——
    /// `get_addr()` 拼的是 `host:port`，那正是 std 认的 IPv6 形式。
    pub url: String,

    /// 主机。留空 = 由 `url` 决定；[`Self::from_str`] 会把解析结果回填到这里。
    #[serde(default)]
    pub host: String,
    /// 端口。`0` = 由 `url` 决定（串里没写则 1433）。
    #[serde(default)]
    pub port: u16,
    /// 登录名 / 密码。留空 = 由 `url` 决定（ADO 形态则由 tiberius 整串解析，
    /// 含 `IntegratedSecurity`）。二者必须成对给出。
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// 数据库名，优先于 `url` / ADO 串里的值。
    #[serde(default)]
    pub database: Option<String>,

    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 会话初始化语句。默认 `SET ARITHABORT ON`；显式 `[]` 表示关闭。
    #[serde(default)]
    pub session_init: Option<Vec<String>>,

    #[serde(default)]
    pub max_connections: Option<u32>,
    /// deadpool **没有** sqlx 的 `min_connections` 概念，本字段仅供 `warm_up()` 用。
    #[serde(default)]
    pub min_connections: Option<u32>,
    /// 取连接的最长等待。注意 **`0` 不是「禁用」** —— deadpool 的
    /// `wait_timeout(Some(Duration::ZERO))` 是**非阻塞**（取不到立即失败），
    /// 与 [`Self::query_timeout_secs`] 的 `0`（禁用）语义不同。
    #[serde(default)]
    pub acquire_timeout_secs: Option<u64>,
    /// `0` = 禁用。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
}

/// 池参数的解析结果（带默认值）。由 [`MssqlConfig::pool`] 产出。
#[derive(Debug, Clone)]
pub struct MssqlParams {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub query_timeout: Option<Duration>,
}

impl Default for MssqlParams {
    /// 文档默认值。只此一处定义，避免与 [`MssqlConfig::pool`] 静默分叉。
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(30),
            query_timeout: Some(Duration::from_secs(30)),
        }
    }
}

/// 连接串的解析结果。
///
/// `username` / `password` 只有 URL 形态有值：ADO 形态的认证（含
/// `IntegratedSecurity`）由 tiberius 自己解析，回填会抹掉「显式覆盖」与
/// 「串里本来就有」的区别。
struct ParsedUrl {
    host: String,
    port: u16,
    database: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

impl MssqlConfig {
    /// 解析连接串，并把结果回填到各字段（URL 形态填 host/port/database/
    /// username/password，ADO 形态填 host/port/database）。
    ///
    /// 是关联函数而非 `FromStr`：本函数只收连接串一种形态，返回项目统一的
    /// [`RdbmsError`]（`FromStr` 要求 `Err: Debug` 那套约束在这里没有意义）。
    ///
    /// URL 形态的查询串（`?encrypt=...` 等）**不支持**：那些开关直接决定加密
    /// 与证书校验，静默忽略等于「配了却没生效」。TLS 请走 [`Self::tls`]。
    // 名字与 `FromStr::from_str` 撞车但**故意**不实现该 trait（见上一段的理由），
    // 关掉这条 lint 而不是为了迎合它把返回值改成 `Infallible`。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, RdbmsError> {
        let t = parse_url(s)?;
        Ok(Self {
            url: s.to_string(),
            host: t.host,
            port: t.port,
            username: t.username,
            password: t.password,
            database: t.database,
            tls: None,
            session_init: None,
            max_connections: None,
            min_connections: None,
            acquire_timeout_secs: None,
            query_timeout_secs: None,
        })
    }

    /// 连接目标：显式字段 > 连接串里的值 > 默认（`url` 永远会被解析一次，
    /// 顺带校验它本身合法）。
    fn target(&self) -> Result<ParsedUrl, RdbmsError> {
        let parsed = parse_url(&self.url)?;
        Ok(ParsedUrl {
            host: if self.host.is_empty() {
                parsed.host
            } else {
                self.host.clone()
            },
            port: if self.port == 0 {
                parsed.port
            } else {
                self.port
            },
            database: self.database.clone().or(parsed.database),
            username: self.username.clone().or(parsed.username),
            password: self.password.clone().or(parsed.password),
        })
    }

    /// 生成 tiberius 的配置（[`tiberius::Config`]）。
    ///
    /// 加密级别（`EncryptionLevel`）**不在此暴露**：tiberius 默认 `Required`
    /// （整条连接必须加密），改它得先想清楚 `Off` / `NotSupported` 的语义与
    /// 默认值，本批次不做 —— 即没配 [`Self::tls`] 也要求加密。
    pub fn build_config(&self) -> Result<Config, RdbmsError> {
        let t = self.target()?;
        let mut cfg = if is_url_form(&self.url) {
            Config::new()
        } else {
            // ADO 串整串交给 tiberius：instance / IntegratedSecurity /
            // TrustServerCertificate 等本结构不建模的键由它处理，下面的字段只做覆盖。
            Config::from_ado_string(&self.url)
                .map_err(|e| RdbmsError::Config(format!("连接串解析失败: {e}")))?
        };

        cfg.host(&t.host);
        cfg.port(t.port);
        match (t.username.as_deref(), t.password.as_deref()) {
            (Some(u), Some(p)) => {
                cfg.authentication(AuthMethod::sql_server(u, p));
            }
            (None, None) => {}
            _ => {
                return Err(RdbmsError::Config(
                    "连接串只给了用户名或只给了密码：SQL Server 登录名与密码必须成对给出".into(),
                ));
            }
        }
        if let Some(db) = t.database.as_deref() {
            cfg.database(db);
        }
        // 与 sqlx 路径 PG 的 `application_name` 同一目的：服务端侧看得出连接来自 ecat。
        // ADO 串里写了 `Application Name` 也会被这里覆盖 —— tiberius 没有 getter，
        // 无从判断用户是否显式设过。
        cfg.application_name("ecat");

        self.apply_tls(&mut cfg)?;
        Ok(cfg)
    }

    /// 把 [`Self::tls`] 映射到 tiberius 的证书开关上。
    ///
    /// 互斥检查必须自己做：tiberius 的 `trust_cert` / `trust_cert_ca` 在两者
    /// 都调用时是 **panic** 而不是返回 `Err`（`config.rs:264,291`），
    /// 配置错误不该让进程崩掉。
    fn apply_tls(&self, cfg: &mut Config) -> Result<(), RdbmsError> {
        let Some(tls) = &self.tls else {
            return Ok(());
        };

        // 与 `TlsClientConfig` 同一条规则：skip_verify 与 ca_cert 同时配置等于
        // 「既要校验又不校验」，必须报错而非静默选一个。
        if tls.skip_verify == Some(true) && tls.ca_cert.is_some() {
            return Err(RdbmsError::Config(
                "tls.skip_verify=true 与 tls.ca_cert 互斥：不能既要跳过证书校验又配置信任锚".into(),
            ));
        }
        if tls.skip_verify == Some(true) {
            cfg.trust_cert();
        }
        if let Some(ca) = &tls.ca_cert {
            cfg.trust_cert_ca(ca);
        }
        match (&tls.client_cert, &tls.client_key) {
            (Some(cert), Some(key)) => {
                cfg.client_certificate(cert, key);
            }
            (None, None) => {}
            _ => {
                return Err(RdbmsError::Config(
                    "tls.client_cert 与 tls.client_key 必须成对配置（半配置的客户端证书用不了）"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    /// 未配置的字段一律取 [`MssqlParams::default`]。
    pub fn pool(&self) -> MssqlParams {
        let d = MssqlParams::default();
        let max_connections = self.max_connections.unwrap_or(d.max_connections);
        MssqlParams {
            max_connections,
            // min 必须 ≤ max：deadpool 没有保底连接概念，本字段只被 `warm_up()`
            // 使用；上限更低时永远取不满，只会阻塞到 acquire_timeout 后报错
            // （与 `SqlxConfig::pool` 同一条规则）。
            min_connections: self
                .min_connections
                .unwrap_or(d.min_connections)
                .min(max_connections),
            acquire_timeout: self
                .acquire_timeout_secs
                .map_or(d.acquire_timeout, Duration::from_secs),
            query_timeout: self.query_timeout(),
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

    /// 会话初始化语句：显式配置优先（空数组 = 主动关闭），未配置时默认
    /// `SET ARITHABORT ON`。
    ///
    /// 为什么默认开：ARITHABORT 取值不同会让同一条查询在 SQL Server 上产生
    /// **两份执行计划**，互相把对方的计划挤出缓存（很多客户端把它设成 OFF，
    /// 服务端默认 ON）。显式 `[]` 表示「我知道我在做什么」。
    pub fn effective_session_init(&self) -> Vec<String> {
        self.session_init
            .clone()
            .unwrap_or_else(|| vec!["SET ARITHABORT ON".to_string()])
    }
}

/// URL 形态判定：复用 [`Dialect`] 的 scheme 识别（已含大小写与首尾空白归一化），
/// 不自己写第二套。
fn is_url_form(url: &str) -> bool {
    Dialect::from_url(url) == Dialect::Mssql
}

/// 解析连接串。URL 形态手拆 —— workspace 里没有 `url` crate，不为这一处解析
/// 引入依赖。语法 `scheme://[user[:pass]@]host[:port][/db]`。
fn parse_url(url: &str) -> Result<ParsedUrl, RdbmsError> {
    // 与 `Dialect::from_url` 同一套谓词（url crate 的 `ch <= ' '`），否则
    // 两边对空白的判定会分叉。
    let s = url.trim_matches(|c: char| c <= ' ');

    if !is_url_form(s) {
        return parse_ado(s, url);
    }

    if s.contains('?') {
        return Err(RdbmsError::Config(format!(
            "连接串不支持查询参数（{url}）：TLS 与证书校验请用 `tls` 字段配置，\
             不做静默忽略"
        )));
    }

    let rest = s
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| RdbmsError::Config(format!("连接串缺少 `://`: {url}")))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));

    // 拆在**最后一个** `@`：与 WHATWG（url crate）一致，前面的 `@` 属于密码。
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => (Some(userinfo), hostport),
        None => (None, authority),
    };
    let (username, password) = match userinfo {
        None => (None, None),
        Some(u) => match u.split_once(':') {
            Some((user, pass)) => (Some(user.to_string()), Some(pass.to_string())),
            None => (Some(u.to_string()), None),
        },
    };

    let (host, port) = split_host_port(hostport);
    if host.is_empty() {
        return Err(RdbmsError::Config(format!("连接串缺少主机名: {url}")));
    }

    Ok(ParsedUrl {
        host,
        port,
        database: (!path.is_empty()).then(|| path.to_string()),
        username,
        password,
    })
}

/// ADO 形态：整串交给 tiberius 的解析器，它报什么错就报什么错。
fn parse_ado(s: &str, url: &str) -> Result<ParsedUrl, RdbmsError> {
    let cfg = Config::from_ado_string(s)
        .map_err(|e| RdbmsError::Config(format!("连接串解析失败（{url}）: {e}")))?;
    let (host, port) = split_host_port(&cfg.get_addr());
    Ok(ParsedUrl {
        host,
        port,
        // tiberius 的 `Config` 只暴露 `get_addr()`，数据库名无从读取，只能回原串取。
        database: ado_value(s, &["database", "initial catalog"]),
        username: None,
        password: None,
    })
}

/// 从 ADO 串里取某个键的值：键名不区分大小写，成对 `{}` 去掉。
///
/// 本函数按 `;` 切分，而 ADO 用 `{}` 包住含 `;` 的值 —— 段内花括号不成对就
/// 说明值被切开了，此时**宁可不回答**（返回 `None`，让 tiberius 自己解析出的
/// 值生效），也不能拿半截值去覆盖：`Database={my;db}` 静默连到 `my` 库是
/// 数据级错误，比不回答严重得多。
fn ado_value(s: &str, keys: &[&str]) -> Option<String> {
    s.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if !keys.contains(&k.trim().to_ascii_lowercase().as_str()) {
            return None;
        }
        let v = v.trim();
        if let Some(inner) = v.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
            return Some(inner.to_string());
        }
        if v.contains(['{', '}']) {
            return None;
        }
        Some(v.to_string())
    })
}

/// `host[:port]` → `(host, port)`。
///
/// 端口缺失或不是数字时整串当主机名、端口取默认 —— IPv6 字面量 `[::1]` 里的
/// 冒号因此不会被误当成分隔符（带端口的 `[::1]:1433` 仍能正确拆开）。
fn split_host_port(s: &str) -> (String, u16) {
    let Some((host, port)) = s.rsplit_once(':') else {
        return (s.to_string(), DEFAULT_PORT);
    };
    match (host.is_empty(), port.parse::<u16>()) {
        (false, Ok(port)) => (host.to_string(), port),
        _ => (s.to_string(), DEFAULT_PORT),
    }
}

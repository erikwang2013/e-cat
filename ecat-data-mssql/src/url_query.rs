// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! URL 形态连接串的查询串解析（`?encrypt=off&trustservercertificate=true`）。
//!
//! 单独成文件是因为 `config.rs` 有 500 行上限；内容上它就是连接串解析的一部分，
//! 由 `config.rs` 的 `parse_url` 调用。
//!
//! **只认两个键**（ADO.NET 的写法）：`encrypt` 与 `trustservercertificate`。
//! 不认识的键、认不出的取值一律**报错** —— 这两个开关直接决定加密与证书校验，
//! 静默忽略 `?encrypt=false` 等于「配了却没生效」：用户以为关了加密，实际还在
//! 要求 TLS，或者反过来。与 `MssqlConfig` 的 `deny_unknown_fields` 同一条规矩。
//!
//! 更细的 TLS 配置（客户端证书、CA 信任锚）不走这里，走 `MssqlConfig::tls`。

use ecat_data::RdbmsError;
use tiberius::EncryptionLevel;

/// 查询串解析出的开关。两键都没给时就是 `Default`（tiberius 的默认行为不变）。
#[derive(Default)]
pub(crate) struct UrlQuery {
    /// `?encrypt=`：未给则不动 tiberius 的默认（`Required`）。
    pub(crate) encrypt: Option<EncryptionLevel>,
    /// `?trustservercertificate=`：true 时跳过证书校验，false 时不动。
    pub(crate) trust_server_certificate: bool,
}

/// 解析查询串（`?encrypt=off&trustservercertificate=true`）。
///
/// 键名与取值都**不区分大小写**（ADO 风格：`TrustServerCertificate=True` 是常见写法）。
/// 认不出的键、缺 `=` 的段、认不出的取值一律报错（见模块文档）。
pub(crate) fn parse_query(q: &str, url: &str) -> Result<UrlQuery, RdbmsError> {
    let mut out = UrlQuery::default();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| RdbmsError::Config(format!("查询参数缺少 `=`（{pair:?}，{url}）")))?;
        match key.trim().to_ascii_lowercase().as_str() {
            "encrypt" => out.encrypt = Some(parse_encrypt(value.trim())?),
            "trustservercertificate" => out.trust_server_certificate = parse_trust(value.trim())?,
            other => {
                return Err(RdbmsError::Config(format!(
                    "不认识的查询参数 {other:?}（{url}）：只支持 `encrypt` 与 \
                     `trustservercertificate`，其它 TLS 开关请用 `tls` 字段配置 —— \
                     不做静默忽略"
                )));
            }
        }
    }
    Ok(out)
}

/// ADO 的 `encrypt` 取值 → tiberius 的 [`EncryptionLevel`]。
///
/// `true` / `yes` / `false` / `no` 的映射**与 tiberius 自己的 ADO 解析器
/// （`Config::from_ado_string`）逐一对齐** —— 这是本表唯一的硬约束：同一条连接串
/// 走 URL 形态与 ADO 形态必须建出同一种连接，否则**用户从配置里看不出自己走的是
/// 哪条路径**（`mssql://` 与 `Server=...` 都是本项目统一的配置形态）。对齐的是
/// ADO.NET（`SqlClient`）的 `Encrypt` 语义：
///
/// - `true`/`yes`/`mandatory`/`required` → `Required`（要求全连接加密）
/// - `false`/`no`/`optional` → `Off`（**只加密登录包**，其后明文；tiberius 的
///   `Off` 正是这个意思 —— 登录后它把传输降级回裸 TCP）
/// - `off`/`disable`/`notsupported` → `NotSupported`（全程不加密）
///
/// `false` 是这张表里最容易写错的一格：写 `Encrypt=false` 的用户要的是「不用全连接
/// 加密（那套证书麻烦）」，而不是「什么都别加密」，也不是「照旧全加密」。映射成
/// `On`（全加密、证书照验）会让 `false` 与 `true` 在用户看来几乎没区别 ——
/// 自签证书照样连不上，而用户认为自己已经关掉了那套校验。
///
/// `strict`（TDS 8.0 严格模式）按 `Required` 处理：tiberius 的严格模式要它自己的
/// `tds80` feature（本 crate 没开，未开时它连 `Encrypt=strict` 的 ADO 串都会拒绝），
/// 而 `Strict` 与 `Required` 都是「要求加密」，区别只在 TLS 握手发生在 prelogin 之前
/// 还是之后 —— 降级成 `Required` 只是放弃那层「服务端必须先支持 TDS 8.0」的保证，
/// 不会把连接变成明文。
fn parse_encrypt(v: &str) -> Result<EncryptionLevel, RdbmsError> {
    Ok(match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "mandatory" | "required" => EncryptionLevel::Required,
        "false" | "no" | "optional" => EncryptionLevel::Off,
        "off" | "disable" | "notsupported" => EncryptionLevel::NotSupported,
        "strict" => EncryptionLevel::Required,
        other => {
            return Err(RdbmsError::Config(format!(
                "encrypt 取值不认识: {other:?}（可用: true/yes/mandatory/required、\
                 false/no/optional、off/disable/notsupported、strict）"
            )));
        }
    })
}

/// ADO 的 `TrustServerCertificate` 取值。`0`/`1` 也收：老 ADO 串里常见。
fn parse_trust(v: &str) -> Result<bool, RdbmsError> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        other => Err(RdbmsError::Config(format!(
            "trustservercertificate 取值不认识: {other:?}（可用: true/yes/1、false/no/0）"
        ))),
    }
}

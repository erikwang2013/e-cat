// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 适配器统一错误类型。

/// 工作流出站调用失败。
///
/// 适配器在把引擎响应转成错误时必须区分「传输/HTTP 失败」（计入熔断）与
/// 「业务性拒绝」（同样是 `Err`，也会计入熔断 —— 熔断器不区分二者）。
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("config: {0}")]
    Config(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("parse: {0}")]
    Parse(String),
    #[error("remote: {0}")]
    Remote(String),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
}

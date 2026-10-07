// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use http::{Request, StatusCode};
use security_rust::Scanner;

pub use security_rust::{AttackCategory, DetectionResult, ScannerBuilder, Severity};

mod layer;
pub use layer::{SecurityBodyLayer, SecurityBodyService, SecurityLayer, SecurityService};

#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("attack blocked: {0}")]
    AttackBlocked(String),
    /// 请求体超过 body_limit 上限（读体阶段即拒绝，不进入扫描）。
    #[error("request body too large")]
    BodyTooLarge,
    #[error("inner error: {0}")]
    Inner(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl SecurityError {
    pub fn to_http_status(&self) -> StatusCode {
        match self {
            Self::AttackBlocked(_) => StatusCode::FORBIDDEN,
            Self::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Inner(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// 拦截结果映射为 HTTP 响应：攻击拦截为 403，请求体超限为 413，内部
/// 错误为 500。内部错误的原始信息只进日志，响应体保持通用文案，避免
/// 把内部错误细节（文件路径、SQL、堆栈）泄露给客户端。
impl axum::response::IntoResponse for SecurityError {
    fn into_response(self) -> axum::response::Response {
        let status = self.to_http_status();
        let body = match &self {
            Self::AttackBlocked(types) => {
                format!(r#"{{"error":"attack blocked","types":"{types}"}}"#)
            }
            Self::BodyTooLarge => r#"{"error":"request body too large"}"#.to_string(),
            Self::Inner(e) => {
                tracing::error!(error = %e, "security middleware internal error");
                r#"{"error":"internal server error"}"#.to_string()
            }
        };
        (status, body).into_response()
    }
}

/// Wraps `security_rust::Scanner` with convenient constructors.
pub struct SecurityScanner {
    scanner: Scanner,
}

impl SecurityScanner {
    /// Create scanner with default detector configuration.
    pub fn new() -> Self {
        Self {
            scanner: Scanner::default(),
        }
    }

    /// Scan a single string through all detectors.
    pub fn scan(&self, input: &str) -> Vec<DetectionResult> {
        self.scanner.scan(input)
    }

    /// Scan multiple request parts (path, headers, body, etc.).
    pub fn scan_parts(&self, parts: &[&str]) -> Vec<DetectionResult> {
        let mut results = Vec::with_capacity(parts.len() * 2);
        for part in parts {
            results.extend(self.scanner.scan(part));
        }
        results
    }

    /// Scan request body bytes. Converts to string for analysis.
    pub fn scan_body(&self, body: &[u8]) -> Vec<DetectionResult> {
        if let Ok(s) = std::str::from_utf8(body) {
            self.scanner.scan(s)
        } else {
            Vec::new()
        }
    }
}

impl Default for SecurityScanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Logs detections and returns a blocking error when a High/Critical attack
/// was found. Shared by the header-scanning and body-scanning middlewares.
fn evaluate(results: &[DetectionResult]) -> Option<SecurityError> {
    let mut blocked = false;
    for r in results {
        tracing::warn!(
            attack_type = %r.attack_type,
            category = ?r.category,
            severity = ?r.severity,
            matched = %r.matched_pattern,
            "attack detected"
        );
        // jwt_attack 的宽正则（ey..ey.. 匹配一切标准 JWT）会误伤合法 token：
        // 服务端鉴权由 JwtAuthLayer 验签把关（alg:none/伪造签名在验签层拒绝），
        // 此处仅记日志不拦截。
        if r.attack_type != "jwt_attack"
            && matches!(r.severity, Severity::High | Severity::Critical)
        {
            blocked = true;
        }
    }
    if blocked {
        let attack_types: Vec<String> = results.iter().map(|r| r.attack_type.to_string()).collect();
        return Some(SecurityError::AttackBlocked(attack_types.join(", ")));
    }
    None
}

/// 百分号解码，仅用于扫描检测；原始 URI 在响应/日志/转发中保持不变。
/// 无效的 % 序列原样保留，非 UTF-8 解码结果按 replacement 字符处理。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Builds the scan list from URI and headers (shared by both middlewares).
/// URI 先做百分号解码再扫描：`?q=SELECT%20*%20FROM%20users` 若不解码会绕过
/// 要求字面空白的 SQLi 正则。解码仅用于检测，URI 本身不变。
/// 代理拓扑头（X-Forwarded-For/X-Real-IP/Forwarded 等）由网关重写，携带
/// 内网 IP（如 docker 网关 172.x），SSRF 检测会误伤内网部署，跳过不扫。
/// Authorization 同理：JWT 是本站正常鉴权流量，jwt_attack 规则会误报，
/// 扫描跳过（token 的校验由 JwtAuthLayer 负责）。
fn is_proxy_header(name: &http::header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "x-forwarded-for"
            | "x-real-ip"
            | "forwarded"
            | "x-forwarded-host"
            | "x-forwarded-proto"
            | "x-forwarded-port"
            | "authorization"
    )
}

fn request_parts<B>(req: &Request<B>) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    parts.push(percent_decode(&req.uri().to_string()));
    for (name, value) in req.headers() {
        if is_proxy_header(name) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            parts.push(v.to_string());
        }
    }
    parts
}

#[cfg(test)]
mod tests;

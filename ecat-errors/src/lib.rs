mod codes;

use codes::ErrorCodeExt;
use ecat_protos::errors::ErrorCode;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub struct Error {
    pub code: ErrorCode,
    pub reason: String,
    pub message: String,
    #[source]
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    pub metadata: HashMap<String, String>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}: {}", self.code, self.reason, self.message)
    }
}

impl Error {
    pub fn new(code: ErrorCode, reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
            message: message.into(),
            cause: None,
            metadata: HashMap::new(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn from_status(status: tonic::Status) -> Self {
        Self::new(ErrorCode::Internal, "grpc_error", status.message().to_string())
    }

    pub fn to_http_status(&self) -> http::StatusCode {
        self.code.http_status()
    }
}

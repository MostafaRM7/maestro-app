use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    AuthenticationRequired,
    CliNotInstalled,
    CliProtocolIncompatible,
    DaemonLocked,
    DatabaseUnavailable,
    InvalidRequest,
    PermissionDenied,
    ProcessCrashed,
    SessionNotFound,
    TerminalLimitReached,
    TerminalNotFound,
    TerminalNotRunning,
    InputTooLarge,
    InvalidPath,
    UnsupportedCapability,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaestroError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub user_action: Option<String>,
    pub correlation_id: uuid::Uuid,
    pub details: Option<serde_json::Value>,
}

impl MaestroError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            user_action: None,
            correlation_id: uuid::Uuid::new_v4(),
            details: None,
        }
    }

    #[must_use]
    pub fn with_user_action(mut self, user_action: impl Into<String>) -> Self {
        self.user_action = Some(user_action.into());
        self
    }
}

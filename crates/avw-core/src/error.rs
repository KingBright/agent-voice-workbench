use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("capacity limit: {0}")]
    Capacity(String),
    #[error("unsupported capability: {0}")]
    Unsupported(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("deadline exceeded")]
    Deadline,
    #[error("integrity check failed: {0}")]
    Integrity(String),
    #[error("model error: {0}")]
    Model(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) | Self::Json(_) | Self::Wav(_) => "invalid_input",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Capacity(_) => "capacity_exceeded",
            Self::Unsupported(_) => "unsupported_capability",
            Self::Cancelled => "cancelled",
            Self::Deadline => "deadline_exceeded",
            Self::Integrity(_) => "integrity_error",
            Self::Model(_) => "model_error",
            Self::Io(_) => "io_error",
            Self::Internal(_) => "internal_error",
        }
    }
    pub fn failure(&self) -> Failure {
        Failure {
            code: self.code().into(),
            message: self.to_string().chars().take(2048).collect(),
            retryable: matches!(self, Self::Capacity(_)),
        }
    }
}

use serde::{Deserialize, Serialize};

/// API error types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApiError {
    /// Network or connection error
    Network(String),
    /// Server returned an error status
    Server { status: u16, message: String },
    /// Failed to parse response
    Parse(String),
    /// Authentication required or failed
    Auth(String),
    /// Resource not found
    NotFound(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Network(msg) => write!(f, "Network error: {}", msg),
            ApiError::Server { status, message } => {
                write!(f, "Server error ({}): {}", status, message)
            }
            ApiError::Parse(msg) => write!(f, "Parse error: {}", msg),
            ApiError::Auth(msg) => write!(f, "Auth error: {}", msg),
            ApiError::NotFound(msg) => write!(f, "Not found: {}", msg),
        }
    }
}

impl std::error::Error for ApiError {}

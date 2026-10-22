//! Error types: the HTTP API's typed error body and the terminal-stream error
//! message.

use serde::{Deserialize, Serialize};

/// What kind of failure an API error is. A client branches on this rather
/// than on the message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    Unauthorized,
    Forbidden,
    NotFound,
    BadRequest,
    Conflict,
    PayloadTooLarge,
    /// An upstream (a launcher, an agent CLI, a provider) failed.
    UpstreamFailed,
    /// An upstream did not answer in time.
    UpstreamTimeout,
    Unavailable,
    Internal,
    /// A code this client doesn't know yet, from a newer server.
    #[serde(other)]
    Unknown,
}

/// The body of a failed API request, sent as JSON only to a request that
/// asked for it (`Accept: application/json`); others get `message` as plain
/// text with the same status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ApiErrorCode,
    /// Safe to show the user as is.
    pub message: String,
    /// Whether the same request may succeed later unchanged. Informational,
    /// for the UI to offer a retry; never a reason to resend a mutation
    /// automatically.
    #[serde(default)]
    pub retryable: bool,
}

/// An error message for display in the terminal output stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMessage {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl ErrorMessage {
    pub fn new(message: String) -> Self {
        Self {
            error_type: "error".to_string(),
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_wire_shape_and_unknown_code() {
        let error = ApiError {
            code: ApiErrorCode::UpstreamTimeout,
            message: "The launcher did not answer".into(),
            retryable: true,
        };
        let wire = serde_json::to_string(&error).unwrap();
        assert_eq!(
            wire,
            r#"{"code":"upstream_timeout","message":"The launcher did not answer","retryable":true}"#
        );
        assert_eq!(serde_json::from_str::<ApiError>(&wire).unwrap(), error);
        // A newer server's code still decodes, and retryable defaults off.
        let newer: ApiError =
            serde_json::from_str(r#"{"code":"rate_limited","message":"Slow down"}"#).unwrap();
        assert_eq!(newer.code, ApiErrorCode::Unknown);
        assert!(!newer.retryable);
    }
}

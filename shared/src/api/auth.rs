//! Authentication request/response types.

use serde::{Deserialize, Serialize};

/// Response from POST /api/auth/refresh.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenRefreshResponse {
    /// True when a replacement token was minted.
    pub refreshed: bool,
    /// Replacement bearer token. Omitted when the current token is not old
    /// enough to refresh yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// Expiry of the replacement token, or the current token when no refresh
    /// was needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

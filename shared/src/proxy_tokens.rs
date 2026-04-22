//! Proxy Token Types
//!
//! Types for JWT-based proxy authentication tokens.
//! These allow the proxy CLI to authenticate without going through
//! the device flow each time.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// JWT claims for proxy authentication tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyTokenClaims {
    /// Token ID (for revocation lookup in proxy_auth_tokens table)
    pub jti: Uuid,
    /// User ID
    pub sub: Uuid,
    /// User email
    pub email: String,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// Expires at (Unix timestamp)
    pub exp: i64,
    /// Token type: "proxy" or "launcher"
    #[serde(default = "default_token_type")]
    pub token_type: String,
}

fn default_token_type() -> String {
    "proxy".to_string()
}

/// Configuration encoded in the init URL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyInitConfig {
    /// JWT token
    #[serde(rename = "t")]
    pub token: String,
    /// Optional session name prefix
    #[serde(rename = "n", skip_serializing_if = "Option::is_none")]
    pub session_name_prefix: Option<String>,
}

impl ProxyInitConfig {
    /// Encode the config as base64url for use in URLs
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(base64_url_encode(json.as_bytes()))
    }

    /// Decode the config from base64url
    pub fn decode(encoded: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let bytes = base64_url_decode(encoded)?;
        let json = String::from_utf8(bytes)?;
        Ok(serde_json::from_str(&json)?)
    }
}

/// Request to create a new proxy token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProxyTokenRequest {
    /// Human-readable name for the token (e.g., "My laptop", "CI runner")
    pub name: String,
    /// Token lifetime in days (default: 30)
    #[serde(default = "default_expires_in_days")]
    pub expires_in_days: u32,
}

fn default_expires_in_days() -> u32 {
    30
}

/// Response after creating a proxy token
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateProxyTokenResponse {
    /// The token ID
    pub id: Uuid,
    /// The JWT token (only shown once)
    pub token: String,
    /// Full init URL for convenience
    pub init_url: String,
    /// When the token expires
    pub expires_at: String,
}

/// Request to renew an existing proxy token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewProxyTokenRequest {
    /// New token lifetime in days (default: 30)
    #[serde(default = "default_expires_in_days")]
    pub expires_in_days: u32,
}

/// Info about an existing proxy token (without the secret)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyTokenInfo {
    pub id: Uuid,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: String,
    pub revoked: bool,
}

/// List of proxy tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyTokenListResponse {
    pub tokens: Vec<ProxyTokenInfo>,
}

// ============================================================================
// Base64 URL-safe encoding/decoding (no external dependency needed)
// ============================================================================

const BASE64_URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64_url_encode(input: &[u8]) -> String {
    let mut result = String::new();
    let mut i = 0;

    while i < input.len() {
        let b0 = input[i] as usize;
        let b1 = if i + 1 < input.len() {
            input[i + 1] as usize
        } else {
            0
        };
        let b2 = if i + 2 < input.len() {
            input[i + 2] as usize
        } else {
            0
        };

        result.push(BASE64_URL_ALPHABET[b0 >> 2] as char);
        result.push(BASE64_URL_ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if i + 1 < input.len() {
            result.push(BASE64_URL_ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        }

        if i + 2 < input.len() {
            result.push(BASE64_URL_ALPHABET[b2 & 0x3f] as char);
        }

        i += 3;
    }

    result
}

fn base64_url_decode(input: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut result = Vec::new();
    let chars: Vec<u8> = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| {
            BASE64_URL_ALPHABET
                .iter()
                .position(|&x| x == c as u8)
                .map(|p| p as u8)
                .ok_or_else(|| format!("Invalid base64url character: {}", c))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut i = 0;
    while i < chars.len() {
        let b0 = chars[i];
        let b1 = if i + 1 < chars.len() { chars[i + 1] } else { 0 };
        let b2 = if i + 2 < chars.len() { chars[i + 2] } else { 0 };
        let b3 = if i + 3 < chars.len() { chars[i + 3] } else { 0 };

        result.push((b0 << 2) | (b1 >> 4));

        if i + 2 < chars.len() {
            result.push((b1 << 4) | (b2 >> 2));
        }

        if i + 3 < chars.len() {
            result.push((b2 << 6) | b3);
        }

        i += 4;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_url_roundtrip() {
        let original = b"Hello, World!";
        let encoded = base64_url_encode(original);
        let decoded = base64_url_decode(&encoded).unwrap();
        assert_eq!(original.to_vec(), decoded);
    }

    #[test]
    fn test_proxy_init_config_roundtrip() {
        let config = ProxyInitConfig {
            token: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9".to_string(),
            session_name_prefix: Some("test-".to_string()),
        };

        let encoded = config.encode().unwrap();
        let decoded = ProxyInitConfig::decode(&encoded).unwrap();

        assert_eq!(config.token, decoded.token);
        assert_eq!(config.session_name_prefix, decoded.session_name_prefix);
    }
}

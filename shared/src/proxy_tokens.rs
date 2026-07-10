//! Proxy Token Types
//!
//! Types for JWT-based proxy authentication tokens.
//! These allow the proxy CLI to authenticate without going through
//! the device flow each time.

use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
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
    /// Expires at (Unix timestamp). `None` means the token never expires; the
    /// backend's live DB checks (revocation, user disabled) govern validity
    /// instead of a fixed TTL. See #932.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
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
    /// `None` for rows without a fixed expiry; launcher credentials may be
    /// rotated to expiring replacements after registration.
    pub expires_at: Option<String>,
    pub revoked: bool,
}

/// List of proxy tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyTokenListResponse {
    pub tokens: Vec<ProxyTokenInfo>,
}

// ============================================================================
// Base64 URL-safe encoding/decoding
// ============================================================================

/// URL-safe base64 engine: encodes without padding; decodes leniently
/// (optional padding, non-canonical trailing bits) to keep accepting every
/// input the previous hand-rolled decoder accepted.
const BASE64_URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

fn base64_url_encode(input: &[u8]) -> String {
    BASE64_URL.encode(input)
}

fn base64_url_decode(input: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    // The previous decoder ignored whitespace anywhere in the input.
    let filtered: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    Ok(BASE64_URL.decode(filtered)?)
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
    fn test_base64_url_roundtrip_all_byte_values() {
        let original: Vec<u8> = (0u8..=255).collect();
        let encoded = base64_url_encode(&original);
        let decoded = base64_url_decode(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_base64_url_known_fixtures() {
        // Outputs must match the previous hand-rolled encoder exactly.
        assert_eq!(base64_url_encode(b""), "");
        assert_eq!(base64_url_encode(b"f"), "Zg");
        assert_eq!(base64_url_encode(b"fo"), "Zm8");
        assert_eq!(base64_url_encode(b"foo"), "Zm9v");
        assert_eq!(base64_url_encode(b"Hello, World!"), "SGVsbG8sIFdvcmxkIQ");
        // Bytes that exercise the URL-safe alphabet characters - and _
        assert_eq!(base64_url_encode(&[0xfb, 0xff]), "-_8");

        assert_eq!(base64_url_decode("Zg").unwrap(), b"f".to_vec());
        assert_eq!(
            base64_url_decode("SGVsbG8sIFdvcmxkIQ").unwrap(),
            b"Hello, World!".to_vec()
        );
        assert_eq!(base64_url_decode("-_8").unwrap(), vec![0xfb, 0xff]);
    }

    #[test]
    fn test_base64_url_decode_leniency() {
        // Whitespace anywhere is ignored (as the old decoder did).
        assert_eq!(
            base64_url_decode("SGVs bG8s\nIFdvcmxkIQ").unwrap(),
            b"Hello, World!".to_vec()
        );
        // Trailing padding is tolerated.
        assert_eq!(
            base64_url_decode("SGVsbG8sIFdvcmxkIQ==").unwrap(),
            b"Hello, World!".to_vec()
        );
        // Non-canonical trailing bits are tolerated (as the old decoder did).
        assert_eq!(base64_url_decode("Zh").unwrap(), b"f".to_vec());
        // Invalid characters are rejected.
        assert!(base64_url_decode("ab$c").is_err());
        // Standard (non-URL-safe) alphabet is rejected.
        assert!(base64_url_decode("+/").is_err());
    }

    #[test]
    fn test_proxy_init_config_decode_fixture() {
        // Encoded form of {"t":"tok","n":"pre"} produced by the previous
        // hand-rolled encoder; must keep decoding.
        let encoded = "eyJ0IjoidG9rIiwibiI6InByZSJ9";
        let decoded = ProxyInitConfig::decode(encoded).unwrap();
        assert_eq!(decoded.token, "tok");
        assert_eq!(decoded.session_name_prefix, Some("pre".to_string()));

        // And the encoder must reproduce it byte-for-byte.
        assert_eq!(decoded.encode().unwrap(), encoded);
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

//! Utility functions for JWT parsing and encoding.

use anyhow::Result;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use shared::ProxyInitConfig;

/// Parse an init value which can be:
/// - A full URL: https://server.com/p/{base64_config}
/// - Just the base64 config part
/// - A raw JWT token
///
/// Returns (backend_url, token)
pub fn parse_init_value(value: &str) -> Result<(Option<String>, String)> {
    // Check if it's a URL
    if value.starts_with("http://") || value.starts_with("https://") {
        return parse_init_url(value);
    }

    // Check if it looks like a JWT (three base64 parts separated by dots)
    if value.contains('.') && value.split('.').count() == 3 {
        return Ok((None, value.to_string()));
    }

    // Try to decode as ProxyInitConfig
    match ProxyInitConfig::decode(value) {
        Ok(config) => Ok((None, config.token)),
        Err(_) => Ok((None, value.to_string())),
    }
}

/// Parse a full init URL
fn parse_init_url(value: &str) -> Result<(Option<String>, String)> {
    use anyhow::Context;

    let url = url::Url::parse(value).context("Invalid init URL")?;

    // WebSocket URL (convert http->ws, https->wss)
    let ws_scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    let ws_url = format!(
        "{}://{}{}",
        ws_scheme,
        url.host_str().unwrap_or("localhost"),
        url.port().map(|p| format!(":{}", p)).unwrap_or_default()
    );

    // Extract config from path (expected: /p/{config})
    let path = url.path();
    if let Some(config_part) = path.strip_prefix("/p/") {
        let config = ProxyInitConfig::decode(config_part)
            .map_err(|e| anyhow::anyhow!("Failed to decode init config from URL: {}", e))?;

        return Ok((Some(ws_url), config.token));
    }

    anyhow::bail!("Invalid init URL format. Expected: https://server.com/p/{{config}}")
}

/// Extract email from JWT without verification (for display purposes only)
pub fn extract_email_from_jwt(token: &str) -> Option<String> {
    // JWT format: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    // Decode payload
    let decoded = base64_url_decode(parts[1]).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;

    json.get("email").and_then(|e| e.as_str()).map(String::from)
}

/// URL-safe base64 engine: decodes leniently (optional padding,
/// non-canonical trailing bits) to keep accepting every input the previous
/// hand-rolled decoder accepted.
const BASE64_URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

fn base64_url_decode(input: &str) -> Result<Vec<u8>> {
    // The previous decoder ignored whitespace and `=` anywhere in the input.
    let filtered: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .collect();
    BASE64_URL
        .decode(filtered)
        .map_err(|e| anyhow::anyhow!("Invalid base64url: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // base64url of {"email":"user@example.com"}
    const PAYLOAD: &str = "eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20ifQ";

    #[test]
    fn test_extract_email_from_jwt() {
        let jwt = format!("header.{}.signature", PAYLOAD);
        assert_eq!(
            extract_email_from_jwt(&jwt),
            Some("user@example.com".to_string())
        );
    }

    #[test]
    fn test_extract_email_from_jwt_padded_payload() {
        // The old decoder stripped `=` anywhere, so padded payloads decoded.
        let jwt = format!("header.{}==.signature", PAYLOAD);
        assert_eq!(
            extract_email_from_jwt(&jwt),
            Some("user@example.com".to_string())
        );
    }

    #[test]
    fn test_extract_email_from_jwt_invalid() {
        assert_eq!(extract_email_from_jwt("not-a-jwt"), None);
        assert_eq!(extract_email_from_jwt("a.$$$.c"), None);
    }

    #[test]
    fn test_base64_url_decode_fixtures() {
        assert_eq!(
            base64_url_decode("SGVsbG8sIFdvcmxkIQ").unwrap(),
            b"Hello, World!".to_vec()
        );
        assert_eq!(base64_url_decode("-_8").unwrap(), vec![0xfb, 0xff]);
        // Whitespace and `=` are ignored, as the old decoder did.
        assert_eq!(
            base64_url_decode("SGVs bG8s\nIFdvcmxkIQ==").unwrap(),
            b"Hello, World!".to_vec()
        );
        assert!(base64_url_decode("ab$c").is_err());
    }
}

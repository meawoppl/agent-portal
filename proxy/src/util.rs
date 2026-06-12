//! Utility functions for JWT parsing and encoding.

use anyhow::Result;
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

    // Decode payload (with padding fix for base64)
    let payload = parts[1];
    let padded = match payload.len() % 4 {
        2 => format!("{}==", payload),
        3 => format!("{}=", payload),
        _ => payload.to_string(),
    };

    let decoded = base64_url_decode(&padded).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;

    json.get("email").and_then(|e| e.as_str()).map(String::from)
}

/// Simple base64url decoder
fn base64_url_decode(input: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let chars: Vec<u8> = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .map(|c| {
            ALPHABET
                .iter()
                .position(|&x| x == c as u8)
                .map(|p| p as u8)
                .ok_or_else(|| anyhow::anyhow!("Invalid base64url character: {}", c))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut result = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let b0 = chars[i];
        let b1 = chars.get(i + 1).copied().unwrap_or(0);
        let b2 = chars.get(i + 2).copied().unwrap_or(0);
        let b3 = chars.get(i + 3).copied().unwrap_or(0);

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

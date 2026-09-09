use serde::de::DeserializeOwned;
use web_sys::window;

/// How [`fetch_json`] should respond to an HTTP 401 (expired/invalid session).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum On401 {
    /// Redirect the browser to the logout endpoint.
    Logout,
    /// Surface the 401 to the caller as `FetchError::Status(401)`.
    Ignore,
}

/// Error from [`fetch_json`], split so callers can branch on HTTP status.
#[derive(Debug)]
pub enum FetchError {
    /// The request could not be sent (network failure, etc.).
    Network(String),
    /// The server responded with a non-success HTTP status.
    Status(u16),
    /// The response body could not be decoded as the expected type.
    Decode(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Network(e) => write!(f, "request failed: {}", e),
            FetchError::Status(code) => write!(f, "HTTP {}", code),
            FetchError::Decode(e) => write!(f, "failed to parse response: {}", e),
        }
    }
}

/// Redirect the browser to the logout endpoint, clearing the session.
pub fn logout() {
    if let Some(window) = window() {
        let _ = window.location().set_href("/api/auth/logout");
    }
}

/// GET an API path (e.g. "/api/sessions") and decode the JSON response.
///
/// `on_401` selects whether an HTTP 401 logs the user out or is returned
/// to the caller like any other error status.
pub async fn fetch_json<T: DeserializeOwned>(path: &str, on_401: On401) -> Result<T, FetchError> {
    let response = gloo_net::http::Request::get(&api_url(path))
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if response.status() == 401 && on_401 == On401::Logout {
        logout();
        return Err(FetchError::Status(401));
    }
    if !response.ok() {
        return Err(FetchError::Status(response.status()));
    }
    response
        .json::<T>()
        .await
        .map_err(|e| FetchError::Decode(e.to_string()))
}

/// Read `(protocol, host)` from the browser location, with per-field fallbacks.
///
/// Returns `None` only when there is no window (e.g. SSR/tests); per-field
/// failures still fall back to local-dev defaults so callers stay total.
fn location_parts() -> Option<(String, String)> {
    let window = window()?;
    let location = window.location();
    let protocol = location.protocol().unwrap_or_else(|_| "http:".to_string());
    let host = location
        .host()
        .unwrap_or_else(|_| "localhost:3000".to_string());
    Some((protocol, host))
}

/// Get the base HTTP URL (e.g., "http://localhost:3000" or "https://myapp.com")
pub fn get_base_url() -> String {
    match location_parts() {
        Some((protocol, host)) => format!("{}//{}", protocol, host),
        None => "http://localhost:3000".to_string(),
    }
}

/// Get the WebSocket URL (e.g., "ws://localhost:3000" or "wss://myapp.com")
pub fn get_ws_url() -> String {
    match location_parts() {
        Some((protocol, host)) => {
            let ws_protocol = if protocol == "https:" { "wss:" } else { "ws:" };
            format!("{}//{}", ws_protocol, host)
        }
        None => "ws://localhost:3000".to_string(),
    }
}

/// Build a full API URL from a path (e.g., "/api/sessions" -> "http://localhost:3000/api/sessions")
pub fn api_url(path: &str) -> String {
    format!("{}{}", get_base_url(), path)
}

/// Serialize `body` into `builder` and send it in one fallible step.
///
/// `RequestBuilder::json` fails when the body doesn't serialize; chaining it
/// behind `.unwrap()` turns that into a panic inside `spawn_local`. Awaiting
/// this instead routes both failures into the caller's existing `Err` arm.
pub async fn send_json(
    builder: gloo_net::http::RequestBuilder,
    body: &impl serde::Serialize,
) -> Result<gloo_net::http::Response, gloo_net::Error> {
    builder.json(body)?.send().await
}

/// Build a full WebSocket URL from a path (e.g., "/ws/client" -> "ws://localhost:3000/ws/client")
pub fn ws_url(path: &str) -> String {
    format!("{}{}", get_ws_url(), path)
}

/// Format a dollar amount with commas (e.g., 1234.56 -> "$1,234.56")
pub fn format_dollars(amount: f64) -> String {
    let formatted = format!("{:.2}", amount);
    let Some((integer, decimal)) = formatted.split_once('.') else {
        return format!("${formatted}");
    };
    let negative = integer.starts_with('-');
    let digits = integer.strip_prefix('-').unwrap_or(integer);
    let mut with_commas = digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| chunk.iter().map(|byte| *byte as char).collect::<String>())
        .collect::<Vec<_>>()
        .join(",");
    if negative {
        with_commas.insert(0, '-');
    }
    format!("${}.{}", with_commas, decimal)
}

/// Format a timestamp string for display (e.g., "2026-01-15 14:30")
pub fn format_timestamp(ts: &str) -> String {
    let date = js_sys::Date::new(&ts.into());
    if date.get_time().is_nan() {
        return ts.to_string();
    }
    format!(
        "{}-{:02}-{:02} {:02}:{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes()
    )
}

/// Extract folder name from path (last path component)
pub fn extract_folder(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
}

/// Parse a semver-ish "MAJOR.MINOR.PATCH" string into a comparable tuple.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Calculate exponential backoff delay for reconnection attempts.
pub fn calculate_backoff(attempt: u32) -> u32 {
    const INITIAL_MS: u32 = 1000;
    const MAX_MS: u32 = 30000;
    INITIAL_MS
        .saturating_mul(2u32.saturating_pow(attempt.min(5)))
        .min(MAX_MS)
}

/// Read a value from browser localStorage, returning `None` when storage is
/// unavailable (no window, storage disabled) or the key is absent.
pub fn storage_get(key: &str) -> Option<String> {
    window()?
        .local_storage()
        .ok()
        .flatten()?
        .get_item(key)
        .ok()
        .flatten()
}

/// Write a value to browser localStorage, silently doing nothing when
/// storage is unavailable or the write fails.
pub fn storage_set(key: &str, value: &str) {
    if let Some(storage) = window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(key, value);
    }
}

/// Remove a key from browser localStorage, silently doing nothing when
/// storage is unavailable.
pub fn storage_remove(key: &str) {
    if let Some(storage) = window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.remove_item(key);
    }
}

pub use shared::fmt::{format_file_size, format_token_count};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_dollars_adds_commas_and_keeps_two_decimals() {
        assert_eq!(format_dollars(0.0), "$0.00");
        assert_eq!(format_dollars(12.3), "$12.30");
        assert_eq!(format_dollars(1_234.56), "$1,234.56");
        assert_eq!(format_dollars(1_234_567.89), "$1,234,567.89");
    }

    #[test]
    fn format_dollars_handles_negative_amounts() {
        assert_eq!(format_dollars(-1_234.5), "$-1,234.50");
    }

    #[test]
    fn backoff_doubles_then_caps_at_30s() {
        assert_eq!(calculate_backoff(0), 1000);
        assert_eq!(calculate_backoff(1), 2000);
        assert_eq!(calculate_backoff(2), 4000);
        assert_eq!(calculate_backoff(5), 30000);
        assert_eq!(calculate_backoff(99), 30000);
    }

    #[test]
    fn parse_version_splits_semver() {
        assert_eq!(parse_version("2.5.92"), Some((2, 5, 92)));
        assert_eq!(parse_version("dev"), None);
        assert_eq!(parse_version("2.5"), None);
    }
}

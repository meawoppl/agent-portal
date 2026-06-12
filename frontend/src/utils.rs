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

/// Get the base HTTP URL (e.g., "http://localhost:3000" or "https://myapp.com")
pub fn get_base_url() -> String {
    let window = window().expect("no global window");
    let location = window.location();

    let protocol = location.protocol().unwrap_or_else(|_| "http:".to_string());
    let host = location
        .host()
        .unwrap_or_else(|_| "localhost:3000".to_string());

    format!("{}//{}", protocol, host)
}

/// Get the WebSocket URL (e.g., "ws://localhost:3000" or "wss://myapp.com")
pub fn get_ws_url() -> String {
    let window = window().expect("no global window");
    let location = window.location();

    let protocol = location.protocol().unwrap_or_else(|_| "http:".to_string());
    let ws_protocol = if protocol == "https:" { "wss:" } else { "ws:" };
    let host = location
        .host()
        .unwrap_or_else(|_| "localhost:3000".to_string());

    format!("{}//{}", ws_protocol, host)
}

/// Build a full API URL from a path (e.g., "/api/sessions" -> "http://localhost:3000/api/sessions")
pub fn api_url(path: &str) -> String {
    format!("{}{}", get_base_url(), path)
}

/// Build a full WebSocket URL from a path (e.g., "/ws/client" -> "ws://localhost:3000/ws/client")
pub fn ws_url(path: &str) -> String {
    format!("{}{}", get_ws_url(), path)
}

/// Format a dollar amount with commas (e.g., 1234.56 -> "$1,234.56")
pub fn format_dollars(amount: f64) -> String {
    let formatted = format!("{:.2}", amount);
    let (integer, decimal) = formatted.split_once('.').unwrap();
    let with_commas: String = integer
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join(",");
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

/// Human-readable file size: `"512 B"`, `"1.5 KB"`, `"2.0 MB"`.
pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Format token count with K/M suffix for readability
pub fn format_token_count(count: i64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- format_file_size ---

    #[test]
    fn format_file_size_renders_bytes_under_one_kib() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(1), "1 B");
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1023), "1023 B");
    }

    #[test]
    fn format_file_size_renders_kib_between_one_kib_and_one_mib() {
        // 1024 is the boundary — exactly at it we cross to KB.
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(1024 * 1024 - 1), "1024.0 KB");
    }

    #[test]
    fn format_file_size_renders_mib_at_or_above_one_mib() {
        assert_eq!(format_file_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_file_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }

    // --- format_token_count ---

    #[test]
    fn format_token_count_magnitudes() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1_000), "1.0K");
        assert_eq!(format_token_count(1_500), "1.5K");
        assert_eq!(format_token_count(999_999), "1000.0K");
        assert_eq!(format_token_count(1_000_000), "1.0M");
        assert_eq!(format_token_count(2_345_678), "2.3M");
    }
}

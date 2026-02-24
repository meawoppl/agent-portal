use web_sys::window;

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

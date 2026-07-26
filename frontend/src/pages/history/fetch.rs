//! Fetch helpers for the `/api/history` endpoints (cookie-authenticated,
//! same-origin).

use shared::api::{parse_history_ndjson, HistoryMessageLine};

/// Error surfaced by every fetch, rendered inline by the calling component.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// Transport failure (offline, connection refused).
    Network(String),
    /// Non-2xx HTTP status.
    Status(u16),
    /// 2xx body that failed to decode into the expected shape.
    Decode(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Network(e) => write!(f, "network error: {e}"),
            FetchError::Status(401) => write!(f, "not signed in (401) — log in and retry"),
            FetchError::Status(404) => write!(f, "not found (404) — archived session missing"),
            FetchError::Status(s) => write!(f, "server returned HTTP {s}"),
            FetchError::Decode(e) => write!(f, "could not parse server response: {e}"),
        }
    }
}

/// `None` = in flight; `Some(Ok/Err)` = settled.
pub type Load<T> = Option<Result<T, FetchError>>;

/// GET a path and decode a JSON body into `T`.
pub async fn fetch_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, FetchError> {
    let response = gloo_net::http::Request::get(path)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if !response.ok() {
        return Err(FetchError::Status(response.status()));
    }
    response
        .json::<T>()
        .await
        .map_err(|e| FetchError::Decode(e.to_string()))
}

/// GET the NDJSON transcript and parse each non-empty line. The whole body is
/// buffered then split; malformed lines are skipped rather than failing the
/// whole transcript.
pub async fn fetch_messages(
    user: &str,
    session: &str,
) -> Result<Vec<HistoryMessageLine>, FetchError> {
    let path = format!("/api/history/sessions/{user}/{session}/messages");
    let response = gloo_net::http::Request::get(&path)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if !response.ok() {
        return Err(FetchError::Status(response.status()));
    }
    let body = response
        .text()
        .await
        .map_err(|e| FetchError::Decode(e.to_string()))?;
    Ok(parse_history_ndjson(&body))
}

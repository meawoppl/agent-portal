//! Typed models and fetch helpers for the archive-viewer JSON API.
//!
//! Field shapes track the pinned #1288 contract (see the crate docs). Manifest
//! fields mirror `backend/src/archive.rs::SessionArchiveManifest` — the
//! `/manifest` endpoint returns that struct's raw JSON — but are redefined here
//! (rather than depending on the native `backend` crate) with `#[serde(default)]`
//! throughout so schema-additive manifests keep deserializing.

use serde::Deserialize;

/// Error surfaced by every fetch, rendered inline by the calling component.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// Transport failure (offline, DNS, CORS, connection refused).
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
            FetchError::Status(404) => write!(f, "not found (404) — archive missing or moved"),
            FetchError::Status(s) => write!(f, "server returned HTTP {s}"),
            FetchError::Decode(e) => write!(f, "could not parse server response: {e}"),
        }
    }
}

/// `GET /api/users` row.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserSummary {
    pub user_id: String,
    #[serde(default)]
    pub owner_email: Option<String>,
    #[serde(default)]
    pub owner_name: Option<String>,
    #[serde(default)]
    pub session_count: i64,
}

impl UserSummary {
    /// Best available human label for a user (name, else email, else id).
    pub fn label(&self) -> String {
        self.owner_name
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| self.owner_email.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| self.user_id.clone())
    }
}

/// `GET /api/sessions` summary row.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub user_id: String,
    #[serde(default)]
    pub session_name: String,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_activity: String,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub media_count: i64,
    #[serde(default)]
    pub models: Vec<String>,
}

/// `GET /api/rollup?group_by=user` row (compact top strip).
///
/// The rollup endpoint (PR 3b) must emit these field names.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RollupRow {
    /// The group key value (e.g. the user id when `group_by=user`).
    #[serde(default)]
    pub group: String,
    /// Human label for the group, when the server can resolve one.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub session_count: i64,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub total_cost_usd: f64,
}

impl RollupRow {
    pub fn display_label(&self) -> String {
        self.label
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.group.clone())
    }
}

/// Token totals sub-object of the manifest (mirrors `ArchiveTokenTotals`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct TokenTotals {
    #[serde(default)]
    pub input: i64,
    #[serde(default)]
    pub output: i64,
    #[serde(default)]
    pub cache_creation: i64,
    #[serde(default)]
    pub cache_read: i64,
    #[serde(default)]
    pub thinking: i64,
    #[serde(default)]
    pub subagent: i64,
}

impl TokenTotals {
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

/// Raw manifest JSON (`GET /api/sessions/{user}/{session}/manifest`).
///
/// A permissive subset of `SessionArchiveManifest`: every field is
/// `#[serde(default)]` and unknown fields are ignored, so newer/older manifest
/// schemas still populate the header card.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub owner_email: String,
    #[serde(default)]
    pub owner_name: Option<String>,
    #[serde(default)]
    pub session_name: String,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub working_directory: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_activity: String,
    #[serde(default)]
    pub archived_at: String,
    #[serde(default)]
    pub tokens: TokenTotals,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub launcher_version: Option<String>,
    #[serde(default)]
    pub archived_by_version: Option<String>,
}

/// One transcript line from the NDJSON stream
/// (`GET /api/sessions/{user}/{session}/messages`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MessageLine {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub agent_type: String,
    /// Raw stored message content, embedded as a JSON value.
    #[serde(default)]
    pub content: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

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

/// GET the NDJSON messages stream and parse each non-empty line.
///
/// At v1 scale the whole body is buffered then split; malformed lines are
/// skipped rather than failing the whole transcript.
pub async fn fetch_messages(user: &str, session: &str) -> Result<Vec<MessageLine>, FetchError> {
    let path = format!("/api/sessions/{user}/{session}/messages");
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

    Ok(parse_ndjson(&body))
}

/// Parse an NDJSON body into message lines, skipping blank/unparseable lines.
pub fn parse_ndjson(body: &str) -> Vec<MessageLine> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<MessageLine>(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ndjson_skips_blank_and_bad_lines() {
        let body = "\
{\"id\":\"a\",\"role\":\"user\",\"created_at\":\"t1\",\"agent_type\":\"claude\",\"content\":{\"type\":\"user\"}}

not json
{\"id\":\"b\",\"role\":\"assistant\",\"created_at\":\"t2\",\"agent_type\":\"claude\",\"content\":{}}
";
        let lines = parse_ndjson(body);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].id, "a");
        assert_eq!(lines[1].role, "assistant");
    }

    #[test]
    fn user_label_prefers_name_then_email_then_id() {
        let with_name = UserSummary {
            user_id: "id".into(),
            owner_email: Some("e@x.com".into()),
            owner_name: Some("Matt".into()),
            session_count: 1,
        };
        assert_eq!(with_name.label(), "Matt");

        let with_email = UserSummary {
            owner_name: None,
            ..with_name.clone()
        };
        assert_eq!(with_email.label(), "e@x.com");

        let bare = UserSummary {
            owner_name: None,
            owner_email: None,
            ..with_name
        };
        assert_eq!(bare.label(), "id");
    }
}

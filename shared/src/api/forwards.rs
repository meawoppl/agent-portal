//! Port-forward registration API types (docs/PORT_FORWARDING.md).
//!
//! Served at `/api/sessions/{id}/forwards` (browser cookie auth) and
//! `/api/agent/sessions/{id}/forwards` (CLI bearer-token auth) — same
//! handlers, same shapes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The session's single active forward.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardInfo {
    pub port: u16,
    /// Fully-formed public URL (`{scheme}://{label}.{domain}/`), stable for
    /// the session's life regardless of which port it points at.
    pub url: String,
    /// Registration time, RFC 3339.
    pub created_at: String,
    /// When true, the URL serves without a portal login (owner opt-in).
    #[serde(default)]
    pub public: bool,
}

/// One of the caller's active forwards, for the Settings ▸ Forwarding tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserForwardInfo {
    pub session_id: Uuid,
    pub session_name: String,
    pub port: u16,
    /// Fully-formed public URL (`{scheme}://{label}.{domain}/`).
    pub url: String,
    pub public: bool,
}

/// Response for `GET /api/forwards` — the caller's active forwards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserForwardsResponse {
    #[serde(default)]
    pub forwards: Vec<UserForwardInfo>,
}

/// Body for `PATCH /api/sessions/{id}/forwards/public`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetForwardPublicRequest {
    pub public: bool,
}

/// An admin-assigned custom subdomain, for the Admin ▸ Subdomains tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomSubdomainInfo {
    pub label: String,
    pub session_id: Uuid,
    pub session_name: String,
    /// Fully-formed public URL (`{scheme}://{label}.{domain}/`).
    pub url: String,
    pub created_at: String,
}

/// Response for `GET /api/admin/subdomains`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomSubdomainsResponse {
    #[serde(default)]
    pub subdomains: Vec<CustomSubdomainInfo>,
}

/// Body for `POST /api/admin/subdomains`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateCustomSubdomainRequest {
    pub session_id: Uuid,
    pub label: String,
}

/// A session with an active forward, for the admin subdomain-assignment picker
/// (`GET /api/admin/forwards`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminForwardInfo {
    pub session_id: Uuid,
    pub session_name: String,
    pub owner_email: String,
    pub port: u16,
    /// The forward's canonical auto-hash URL.
    pub url: String,
}

/// Response for `GET /api/admin/forwards`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminForwardsResponse {
    #[serde(default)]
    pub forwards: Vec<AdminForwardInfo>,
}

/// Body for `POST …/forwards`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateForwardRequest {
    pub port: u16,
}

/// Response for `POST …/forwards`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateForwardResponse {
    pub forward: ForwardInfo,
    /// The port a pre-existing forward was pointing at, when this call moved
    /// it to a different port (a session has at most one forward). `None` on a
    /// fresh forward or an idempotent same-port call. The CLI surfaces this so
    /// the agent knows its old port stopped forwarding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_port: Option<u16>,
    /// Probe-dial result relayed from the proxy's `ForwardStatus` reply:
    /// `Some(true)` = something is listening on the port, `Some(false)` =
    /// nothing there yet, `None` = no reply (proxy disconnected or predates
    /// forwarding support). The CLI prints a warning for the latter two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening: Option<bool>,
    /// Probe error detail when `listening` is `Some(false)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<String>,
}

/// Response for `GET …/forwards`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionForwardsResponse {
    #[serde(default)]
    pub forwards: Vec<ForwardInfo>,
}

//! Port-forward registration API types (docs/PORT_FORWARDING.md).
//!
//! Served at `/api/sessions/{id}/forwards` (browser cookie auth) and
//! `/api/agent/sessions/{id}/forwards` (CLI bearer-token auth) — same
//! handlers, same shapes.

use serde::{Deserialize, Serialize};

/// The session's single active forward.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardInfo {
    pub port: u16,
    /// Fully-formed public URL (`{scheme}://{label}.{domain}/`), stable for
    /// the session's life regardless of which port it points at.
    pub url: String,
    /// Registration time, RFC 3339.
    pub created_at: String,
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

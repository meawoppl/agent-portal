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
    /// Fully-formed public URL (`{scheme}://{label}.{domain}/`). Stable for a
    /// given port: re-registering the same port keeps the URL; the label only
    /// rotates when the port changes (a different local service), to shed stale
    /// browser origin state.
    pub url: String,
    /// Registration time, RFC 3339.
    pub created_at: String,
    /// When true, the URL serves without a portal login (owner opt-in).
    #[serde(default)]
    pub public: bool,
    /// Latest probe verdict for the forwarded port: `Some(true)` = something
    /// is listening, `Some(false)` = nothing there, `None` = unknown (proxy
    /// disconnected, pre-probe, or pre-2.13.17). Drives the chip tint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening: Option<bool>,
    /// Name of the process bound to the port (e.g. `python3`), when the
    /// probe could resolve it. Shown in the chip tooltip / preview title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
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

/// A recent forward failure, surfaced back to the owning agent so it can see
/// what the browser saw instead of debugging blind (docs/PORT_FORWARDING.md,
/// #1476). Newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardFailure {
    /// Stable [`ForwardError`] code (e.g. `no-listener`, `agent-offline`).
    pub code: String,
    /// The port the failed request targeted.
    pub port: u16,
    /// When it happened, RFC 3339.
    pub at: String,
}

/// Response for `GET …/forwards`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionForwardsResponse {
    #[serde(default)]
    pub forwards: Vec<ForwardInfo>,
    /// Recent failures routing to this session's forward, newest first — the
    /// agent-visible half of the error taxonomy (#1476). Empty when there have
    /// been none since the backend last (re)started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_failures: Vec<ForwardFailure>,
}

/// The single taxonomy of forward failures (docs/PORT_FORWARDING.md).
///
/// **Legibility is the point.** Every failure on the forward control/data path
/// maps to exactly one variant, so no failure can ever reach a user — or the
/// agent that caused it — as a bare, cause-free string. One variant carries
/// everything the three audiences need, rendered from a single source:
///
/// * the browser error page (`title` + `detail`),
/// * the CLI / agent-facing note (`code` + `detail`),
/// * the HTTP boundary (`http_status`, plus the `code` echoed in the
///   `x-portal-forward-error` header so a client can branch without scraping
///   prose).
///
/// The `code` is a stable kebab slug (also the serde wire form) — safe to
/// alert on and match against; the prose may be reworded freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForwardError {
    /// Forwarding isn't configured on this server (no forward domain set).
    NotConfigured,
    /// A backend dependency (database) was unavailable while routing.
    Unavailable,
    /// The subdomain doesn't resolve to any session.
    UnknownForward,
    /// The forward existed but was closed or replaced by a newer one.
    Revoked,
    /// The request needs a portal login / a valid handoff token.
    AuthRequired,
    /// The forward-origin session cookie is missing, invalid, or expired.
    SessionExpired,
    /// The agent's proxy isn't connected — there is no tunnel to route through.
    AgentOffline,
    /// The agent is registered but didn't answer the open in time — almost
    /// always its connection dropped and hasn't been reaped, or it is
    /// reconnecting.
    AgentUnreachable,
    /// The tunnel tore down mid-open before the stream was established.
    OpenInterrupted,
    /// Nothing is listening on the forwarded port on the agent's machine.
    NoListener,
    /// The forward is momentarily at its concurrent-connection limit.
    AtCapacity,
    /// The port is not in the agent's forward allowlist (revoked / re-pointed).
    NotForwarded,
    /// The local server spoke malformed HTTP, or the upstream handshake/response
    /// could not be completed.
    BadOrigin,
    /// A transient tunnel/protocol fault (should not happen in normal use).
    ProtocolFault,
    /// The incoming request line or headers were malformed.
    BadRequest,
}

impl ForwardError {
    /// Stable kebab-case identifier. Matches the serde wire form; echoed in the
    /// `x-portal-forward-error` response header. Safe to alert/branch on.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "not-configured",
            Self::Unavailable => "unavailable",
            Self::UnknownForward => "unknown-forward",
            Self::Revoked => "revoked",
            Self::AuthRequired => "auth-required",
            Self::SessionExpired => "session-expired",
            Self::AgentOffline => "agent-offline",
            Self::AgentUnreachable => "agent-unreachable",
            Self::OpenInterrupted => "open-interrupted",
            Self::NoListener => "no-listener",
            Self::AtCapacity => "at-capacity",
            Self::NotForwarded => "not-forwarded",
            Self::BadOrigin => "bad-origin",
            Self::ProtocolFault => "protocol-fault",
            Self::BadRequest => "bad-request",
        }
    }

    /// HTTP status to return at the forward-origin boundary (raw `u16` so this
    /// stays WASM-safe — the backend maps it to a `StatusCode`).
    pub fn http_status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::AuthRequired | Self::SessionExpired => 401,
            Self::UnknownForward | Self::Revoked | Self::NotForwarded => 404,
            Self::NotConfigured | Self::Unavailable | Self::AgentOffline | Self::AtCapacity => 503,
            Self::AgentUnreachable => 504,
            Self::OpenInterrupted | Self::NoListener | Self::BadOrigin | Self::ProtocolFault => 502,
        }
    }

    /// Short heading for the browser error page.
    pub fn title(self) -> &'static str {
        match self {
            Self::NotConfigured => "Forwarding not available",
            Self::Unavailable => "Temporarily unavailable",
            Self::UnknownForward => "No such forward",
            Self::Revoked => "This forward was closed",
            Self::AuthRequired => "Sign-in required",
            Self::SessionExpired => "Session expired",
            Self::AgentOffline => "Agent offline",
            Self::AgentUnreachable => "Agent not responding",
            Self::OpenInterrupted => "Connection interrupted",
            Self::NoListener => "Nothing is listening",
            Self::AtCapacity => "Too many connections",
            Self::NotForwarded => "Port not forwarded",
            Self::BadOrigin => "Local server error",
            Self::ProtocolFault => "Forwarding error",
            Self::BadRequest => "Bad request",
        }
    }

    /// One or two sentences: what happened, and what to do about it. Written for
    /// the human at the browser and the agent reading it back over the CLI.
    pub fn detail(self) -> &'static str {
        match self {
            Self::NotConfigured => {
                "Port forwarding is not configured on this server, so this URL cannot route anywhere."
            }
            Self::Unavailable => {
                "The portal could not reach a dependency it needs to route this forward. Try again in a moment."
            }
            Self::UnknownForward => {
                "This forwarding URL does not match any active session. It may have been closed, or the link may be mistyped."
            }
            Self::Revoked => {
                "The forward that used this URL was closed or replaced by a newer one. Ask the agent for its current forward URL."
            }
            Self::AuthRequired => {
                "This forward is private. Open it from the portal (or sign in) to get a forward-session cookie first."
            }
            Self::SessionExpired => {
                "Your forward session has expired. Reopen the forward from the portal to refresh it."
            }
            Self::AgentOffline => {
                "The agent that serves this forward is not connected, so there is nothing to tunnel to. It may be starting up, reconnecting, or stopped."
            }
            Self::AgentUnreachable => {
                "The agent is registered but did not answer in time — its connection most likely dropped and has not been cleaned up yet, or it is reconnecting. Retry shortly."
            }
            Self::OpenInterrupted => {
                "The tunnel closed while the connection was being opened, usually because the agent reconnected mid-request. Retry."
            }
            Self::NoListener => {
                "Nothing is listening on the forwarded port on the agent's machine. Confirm the local server is running and bound to that port."
            }
            Self::AtCapacity => {
                "This forward is momentarily at its concurrent-connection limit. Retry in a moment; a page reload usually succeeds."
            }
            Self::NotForwarded => {
                "This port is not currently forwarded for the session. The forward may have moved to a different port."
            }
            Self::BadOrigin => {
                "The local server returned a response the proxy could not understand (malformed HTTP, or the connection failed mid-response)."
            }
            Self::ProtocolFault => {
                "The forwarding tunnel hit an internal protocol error. Retry; if it persists, report it."
            }
            Self::BadRequest => {
                "The request could not be forwarded because its path or headers were malformed."
            }
        }
    }
}

#[cfg(test)]
mod forward_error_tests {
    use super::ForwardError;

    const ALL: &[ForwardError] = &[
        ForwardError::NotConfigured,
        ForwardError::Unavailable,
        ForwardError::UnknownForward,
        ForwardError::Revoked,
        ForwardError::AuthRequired,
        ForwardError::SessionExpired,
        ForwardError::AgentOffline,
        ForwardError::AgentUnreachable,
        ForwardError::OpenInterrupted,
        ForwardError::NoListener,
        ForwardError::AtCapacity,
        ForwardError::NotForwarded,
        ForwardError::BadOrigin,
        ForwardError::ProtocolFault,
        ForwardError::BadRequest,
    ];

    #[test]
    fn codes_are_unique_and_match_serde_wire_form() {
        let mut seen = std::collections::HashSet::new();
        for e in ALL {
            assert!(seen.insert(e.code()), "duplicate code {}", e.code());
            // serde encodes to the kebab code, so the wire form and the stable
            // identifier can never drift.
            let wire = serde_json::to_value(e).unwrap();
            assert_eq!(wire, serde_json::Value::String(e.code().to_string()));
            assert_eq!(
                serde_json::from_value::<ForwardError>(wire).unwrap(),
                *e,
                "round-trip failed for {}",
                e.code()
            );
        }
    }

    #[test]
    fn every_variant_has_a_sane_status_and_nonempty_prose() {
        for e in ALL {
            let s = e.http_status();
            assert!((400..=599).contains(&s), "{} bad status {s}", e.code());
            assert!(!e.title().is_empty(), "{} empty title", e.code());
            assert!(!e.detail().is_empty(), "{} empty detail", e.code());
        }
    }
}

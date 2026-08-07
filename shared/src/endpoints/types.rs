use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgentType, PermissionSuggestion, SessionMode};

/// Why a session continuation was created.
///
/// Stored in `session_continuations.reason` and carried on the wire in
/// [`ContinuationConfig::reason`] (backend → launcher), so this enum lives in
/// `shared`. The launcher uses it to decide the reset skew (see
/// [`ContinuationConfig::reason`]).
///
/// The wire/DB representation is the snake-case string emitted by
/// [`ContinuationReason::as_wire`] — the same values serde produces — so the
/// column and JSON stay in lockstep.
///
/// **Unknown-value policy:** [`ContinuationReason::from_wire`] returns `None`
/// for an unrecognized string rather than panicking. Consumers treat an unknown
/// reason as the neutral `Limit` default (the historical fallthrough), so a
/// legacy or newer-launcher value never breaks scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationReason {
    /// A usage-limit reset (`#231`/`#1260`). Waits the full reset skew.
    Limit,
    /// Auto-retry of a turn a transient 529 overload killed. Fires immediately
    /// (no reset skew) — the CLI already backs off internally, so the portal
    /// adds no further delay.
    Overloaded,
}

impl ContinuationReason {
    /// The wire/DB string for this reason — identical to the serde encoding, so
    /// the `session_continuations.reason` column stays in lockstep with JSON.
    pub const fn as_wire(self) -> &'static str {
        match self {
            ContinuationReason::Limit => "limit",
            ContinuationReason::Overloaded => "overloaded",
        }
    }

    /// Parse a stored/wire reason string. Returns `None` for an unrecognized
    /// value so callers can apply the `Limit`-default fallthrough (see the type
    /// doc's unknown-value policy) rather than panicking on legacy/corrupt data.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "limit" => Some(ContinuationReason::Limit),
            "overloaded" => Some(ContinuationReason::Overloaded),
            _ => None,
        }
    }
}

impl std::fmt::Display for ContinuationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// A session continuation created for a usage-limit reset (`#231`/`#1260`).
pub const CONTINUATION_REASON_LIMIT: &str = ContinuationReason::Limit.as_wire();
/// A session continuation created to auto-retry a turn that a transient 529
/// overload killed. Fires immediately (no reset skew) — the CLI already backs
/// off internally, so the portal adds no further delay.
pub const CONTINUATION_REASON_OVERLOADED: &str = ContinuationReason::Overloaded.as_wire();

/// Fields for session registration (shared by proxy and web client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterFields {
    pub session_id: Uuid,
    pub session_name: String,
    pub auth_token: Option<String>,
    pub working_directory: String,
    #[serde(default)]
    pub resuming: bool,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub replay_after: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub replaces_session_id: Option<Uuid>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub launcher_id: Option<Uuid>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub scheduled_task_id: Option<Uuid>,
    #[serde(default)]
    pub claude_args: Vec<String>,
    /// Optional protocol features this client supports, mirroring the launcher
    /// capability model (`crate::PROXY_CAPABILITY_*`). Defaults to empty, so an
    /// older proxy — which sends no capabilities — is simply treated as
    /// supporting none and keeps the pre-existing behavior.
    ///
    /// Before this, the session socket had no capability mechanism at all: new
    /// frames relied purely on `#[serde(default)]` tolerance, which cannot
    /// express "this peer can receive a frame shape it never used to get".
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Core scheduled-task fields shared by `ScheduledTaskConfig`,
/// `CreateScheduledTaskRequest`, and `ScheduledTaskInfo` via `#[serde(flatten)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTaskFields {
    pub name: String,
    pub cron_expression: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    pub working_directory: String,
    pub prompt: String,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default = "default_max_runtime_minutes")]
    pub max_runtime_minutes: i32,
    /// Whether each firing starts fresh or continues the prior conversation.
    /// Defaults to `Fresh` so payloads from older clients keep today's behavior.
    #[serde(default)]
    pub session_mode: SessionMode,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_max_runtime_minutes() -> i32 {
    30
}

/// Configuration for a scheduled task, sent from backend to launcher via ScheduleSync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTaskConfig {
    pub id: Uuid,
    #[serde(flatten)]
    pub fields: ScheduledTaskFields,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationConfig {
    pub id: Uuid,
    pub session_id: Uuid,
    pub reset_at: String,
    pub prompt: String,
    /// Launch metadata for resuming the same session if the original local
    /// process has already exited by the time the continuation is due.
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    /// `CONTINUATION_REASON_LIMIT` (default, wire-compatible with older
    /// launchers/backends that omit it) or `CONTINUATION_REASON_OVERLOADED`.
    /// The launcher uses this to decide the reset skew: limit resets wait
    /// `CONTINUATION_RESET_SKEW_SECS` past `reset_at`; overload retries fire at
    /// `reset_at` with no skew.
    #[serde(default = "crate::default_continuation_reason")]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLimitContinuationFields {
    pub session_id: Uuid,
    pub reset_at: String,
    pub source_message: String,
    pub prompt: String,
}

/// Fields for a permission response (shared by server-to-proxy and client-to-server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponseFields {
    pub request_id: String,
    pub allow: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PermissionSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Fields for starting a file upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadStartFields {
    pub upload_id: String,
    pub filename: String,
    pub content_type: String,
    pub total_chunks: u32,
    #[serde(default)]
    pub total_size: u64,
}

/// Fields for a single file upload chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadChunkFields {
    pub upload_id: String,
    pub chunk_index: u32,
    pub data: String,
}

/// Terminal outcome of a file upload (#939 phase 4).
///
/// Emitted by the proxy once the file is fully written and renamed into
/// place (or has definitively failed), relayed by the backend to the web
/// client — which withholds the prompt referencing the file until every
/// upload it names has committed. The backend also synthesizes failures it
/// can detect itself (proxy offline, size-cap abort).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadResultFields {
    pub upload_id: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---- Port forwarding (docs/PORT_FORWARDING.md) ------------------------------

/// Open a tunnel stream to `127.0.0.1:{port}` on the proxy host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelOpenFields {
    pub stream_id: Uuid,
    pub port: u16,
}

/// A chunk of stream bytes, base64-encoded, at most 16 KiB decoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelDataFields {
    pub stream_id: Uuid,
    pub data_base64: String,
}

/// Grant the peer `add_bytes` more send credit on a stream (credit-based flow
/// control; each direction starts with a 256 KiB window).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelWindowFields {
    pub stream_id: Uuid,
    pub add_bytes: u32,
}

/// Tear down a stream (no half-close). `reason` is diagnostic only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelCloseFields {
    pub stream_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A stream the proxy successfully dialed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelStreamFields {
    pub stream_id: Uuid,
}

/// Why the proxy could not open a tunnel stream. The backend maps this to the
/// right HTTP status and decides whether it reflects on port health — only
/// `NoListener` means the local service is actually down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelRefuseReason {
    /// The loopback dial to `127.0.0.1:{port}` failed (connection refused or
    /// timed out) — nothing is serving there right now.
    NoListener,
    /// The proxy has hit its concurrent-stream cap for this connection; the
    /// port is fine, the tunnel is momentarily saturated.
    StreamLimit,
    /// The port is not in the proxy's forward allowlist (revoked/re-pointed).
    NotForwarded,
    /// Protocol misuse (duplicate stream id, oversize frame) — should not
    /// happen in normal operation.
    Protocol,
}

impl std::fmt::Display for TunnelRefuseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::NoListener => "nothing is listening on the forwarded port",
            Self::StreamLimit => "the forward is at its concurrent-connection limit",
            Self::NotForwarded => "this port is not forwarded",
            Self::Protocol => "forward protocol error",
        };
        f.write_str(s)
    }
}

/// The proxy could not (or refused to) dial a stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelRefusedFields {
    pub stream_id: Uuid,
    pub reason: TunnelRefuseReason,
}

/// Add/remove a port in the proxy's forward allowlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardPortFields {
    pub port: u16,
}

/// Proxy's reply to `ForwardOpen` (and unsolicited background-probe reports):
/// the allowlist was updated, and a probe dial to `127.0.0.1:{port}` reported
/// whether anything is listening yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardStatusFields {
    pub port: u16,
    pub listening: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Name of the process bound to the port (e.g. `python3`, `vite`), when
    /// the probe found a listener and could resolve its owner. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadRequestFields {
    pub request_id: Uuid,
    pub path: String,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadResponseFields {
    pub request_id: Uuid,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Retry state for a sub-agent whose API call is being retried after an error,
/// carried on the live tool-progress side-channel (#1474).
///
/// Mapped from `claude_codes::SubagentRetry` at the classifier boundary rather
/// than embedding the SDK struct: the portal's `ToolProgress` wire shape has
/// always flattened the SDK's fields (it carries `tool_use_id` / `tool_name` /
/// `elapsed_time_seconds`, not a `ToolProgressMessage`), and the SDK type
/// derives no `PartialEq`, which the frontend's live-status state needs.
///
/// Only the display-relevant fields are carried. The SDK's `agent_id`,
/// `retry_delay_ms` and `error_status` are deliberately dropped — nothing
/// renders them, and this frame is emitted every ~30s per running tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRetryStatus {
    /// 1-based attempt number currently in flight.
    pub attempt: u64,
    /// Total attempts allowed before the sub-agent gives up.
    pub max_retries: u64,
    /// Coarse reason the previous attempt failed (e.g. `overloaded`), as
    /// classified by the CLI. Rendered verbatim, so treat it as opaque.
    pub error_category: String,
}

#[cfg(test)]
mod continuation_reason_tests {
    use super::*;

    #[test]
    fn as_wire_and_serde_agree() {
        for reason in [ContinuationReason::Limit, ContinuationReason::Overloaded] {
            // The wire string, the serde encoding, and Display must all match.
            assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_wire());
            assert_eq!(reason.to_string(), reason.as_wire());
        }
    }

    #[test]
    fn from_wire_round_trips_every_variant() {
        for reason in [ContinuationReason::Limit, ContinuationReason::Overloaded] {
            assert_eq!(
                ContinuationReason::from_wire(reason.as_wire()),
                Some(reason)
            );
        }
    }

    #[test]
    fn from_wire_returns_none_for_unknown() {
        // Unknown-value policy: never panic, hand back None so callers apply the
        // Limit-default fallthrough.
        assert_eq!(ContinuationReason::from_wire("bogus"), None);
        assert_eq!(ContinuationReason::from_wire(""), None);
    }

    #[test]
    fn legacy_consts_match_enum_wire() {
        assert_eq!(
            CONTINUATION_REASON_LIMIT,
            ContinuationReason::Limit.as_wire()
        );
        assert_eq!(
            CONTINUATION_REASON_OVERLOADED,
            ContinuationReason::Overloaded.as_wire()
        );
    }
}

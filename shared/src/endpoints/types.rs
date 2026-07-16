use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgentType, PermissionSuggestion};

/// A session continuation created for a usage-limit reset (`#231`/`#1260`).
pub const CONTINUATION_REASON_LIMIT: &str = "limit";
/// A session continuation created to auto-retry a turn that a transient 529
/// overload killed. Fires immediately (no reset skew) — the CLI already backs
/// off internally, so the portal adds no further delay.
pub const CONTINUATION_REASON_OVERLOADED: &str = "overloaded";

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

/// The proxy could not (or refused to) dial a stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelRefusedFields {
    pub stream_id: Uuid,
    pub error: String,
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

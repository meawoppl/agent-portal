use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgentType, PermissionSuggestion};

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

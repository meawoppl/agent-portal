//! Scheduled-task API request/response types.

use serde::{Deserialize, Serialize};

/// Request to create a scheduled task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateScheduledTaskRequest {
    #[serde(flatten)]
    pub fields: crate::endpoints::ScheduledTaskFields,
    pub hostname: String,
}

/// Request to update a scheduled task (all fields optional)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateScheduledTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<crate::AgentType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_mode: Option<crate::SessionMode>,
}

/// Info about a scheduled task (returned by list/create endpoints)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTaskInfo {
    pub id: uuid::Uuid,
    #[serde(flatten)]
    pub fields: crate::endpoints::ScheduledTaskFields,
    pub hostname: String,
    pub enabled: bool,
    pub last_session_id: Option<uuid::Uuid>,
    pub last_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Response listing scheduled tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTaskListResponse {
    pub tasks: Vec<ScheduledTaskInfo>,
}

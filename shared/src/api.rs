//! API client types and trait definitions
//!
//! This module defines the API contract that can be implemented
//! by both native (reqwest) and WASM (gloo-net) HTTP clients.

use serde::{Deserialize, Serialize};

// Re-export types from parent module for convenience
pub use crate::{DevicePollResponse, MessageInfo, SessionInfo, UserInfo};

/// API error types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApiError {
    /// Network or connection error
    Network(String),
    /// Server returned an error status
    Server { status: u16, message: String },
    /// Failed to parse response
    Parse(String),
    /// Authentication required or failed
    Auth(String),
    /// Resource not found
    NotFound(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Network(msg) => write!(f, "Network error: {}", msg),
            ApiError::Server { status, message } => {
                write!(f, "Server error ({}): {}", status, message)
            }
            ApiError::Parse(msg) => write!(f, "Parse error: {}", msg),
            ApiError::Auth(msg) => write!(f, "Auth error: {}", msg),
            ApiError::NotFound(msg) => write!(f, "Not found: {}", msg),
        }
    }
}

impl std::error::Error for ApiError {}

/// Request to create a proxy auth token
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreateProxyTokenRequest {
    pub session_name_prefix: Option<String>,
}

/// Response from creating a proxy auth token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProxyTokenResponse {
    pub token: String,
    pub expires_at: String,
    pub setup_command: String,
    pub setup_url: String,
}

/// Health check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: Option<String>,
}

/// Device flow code request response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Request to launch a session via a launcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub working_directory: String,
    #[serde(default)]
    pub launcher_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: crate::AgentType,
}

/// Request body for device code creation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceCodeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

/// Request body for polling device flow status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFlowPollRequest {
    pub device_code: String,
}

/// Response for device flow approve/deny actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFlowActionResponse {
    pub success: bool,
    pub message: String,
}

/// Request to update a user's admin/ban/voice settings (admin endpoint)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_admin: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_double_option",
        serialize_with = "serialize_double_option"
    )]
    pub ban_reason: Option<Option<String>>,
}

fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // If the field is present, deserialize its value (which may be null)
    Ok(Some(Option::deserialize(deserializer)?))
}

fn serialize_double_option<S>(
    value: &Option<Option<String>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(inner) => inner.serialize(serializer),
    }
}

/// Request to add a member to a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddMemberRequest {
    pub email: String,
    pub role: String,
}

/// Request to update a session member's role
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMemberRoleRequest {
    pub role: String,
}

/// An error message for display in the terminal output stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMessage {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl ErrorMessage {
    pub fn new(message: String) -> Self {
        Self {
            error_type: "error".to_string(),
            message,
        }
    }
}

/// Permission answer payload sent with PermissionResponse
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionAnswers {
    pub answers: serde_json::Map<String, serde_json::Value>,
}

impl PermissionAnswers {
    pub fn empty() -> Self {
        Self {
            answers: serde_json::Map::new(),
        }
    }
}

/// Response for GET /api/settings/sound
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundSettingsResponse {
    pub sound_config: Option<serde_json::Value>,
}

// =============================================================================
// Scheduled Tasks API Types
// =============================================================================

/// Request to create a scheduled task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateScheduledTaskRequest {
    pub name: String,
    pub cron_expression: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    pub hostname: String,
    pub working_directory: String,
    pub prompt: String,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: crate::AgentType,
    #[serde(default = "default_max_runtime")]
    pub max_runtime_minutes: i32,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_max_runtime() -> i32 {
    30
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
}

/// Info about a scheduled task (returned by list/create endpoints)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTaskInfo {
    pub id: uuid::Uuid,
    pub name: String,
    pub cron_expression: String,
    pub timezone: String,
    pub hostname: String,
    pub working_directory: String,
    pub prompt: String,
    pub claude_args: Vec<String>,
    pub agent_type: crate::AgentType,
    pub enabled: bool,
    pub max_runtime_minutes: i32,
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

/// API endpoint definitions
pub mod endpoints {
    pub const HEALTH: &str = "/";
    pub const AUTH_ME: &str = "/auth/me";
    pub const AUTH_LOGOUT: &str = "/auth/logout";
    pub const SESSIONS: &str = "/api/sessions";
    pub const PROXY_TOKENS: &str = "/api/proxy-tokens";
    pub const DEVICE_CODE: &str = "/auth/device/code";
    pub const DEVICE_POLL: &str = "/auth/device/poll";
    pub const SOUND_SETTINGS: &str = "/api/settings/sound";
    pub const SCHEDULED_TASKS: &str = "/api/scheduled-tasks";

    pub fn session(id: &str) -> String {
        format!("/api/sessions/{}", id)
    }

    pub fn session_messages(id: &str) -> String {
        format!("/api/sessions/{}/messages", id)
    }
}

/// Trait defining the cc-proxy API
///
/// This trait can be implemented by both native and WASM HTTP clients.
/// All methods are async and return Result<T, ApiError>.
#[allow(async_fn_in_trait)]
pub trait CcProxyApi {
    /// Check if the server is healthy
    async fn health(&self) -> Result<HealthResponse, ApiError>;

    /// Get the current authenticated user
    async fn get_me(&self) -> Result<UserInfo, ApiError>;

    /// List all sessions for the current user
    async fn list_sessions(&self) -> Result<Vec<SessionInfo>, ApiError>;

    /// Get a specific session by ID
    async fn get_session(&self, id: &str) -> Result<SessionInfo, ApiError>;

    /// Delete a session
    async fn delete_session(&self, id: &str) -> Result<(), ApiError>;

    /// Create a new proxy authentication token
    async fn create_proxy_token(
        &self,
        req: CreateProxyTokenRequest,
    ) -> Result<CreateProxyTokenResponse, ApiError>;

    /// Request a device code for CLI authentication
    async fn request_device_code(&self) -> Result<DeviceCodeResponse, ApiError>;

    /// Poll for device flow completion
    async fn poll_device_code(&self, device_code: &str) -> Result<DevicePollResponse, ApiError>;
}

/// Configuration for creating an API client
#[derive(Debug, Clone)]
pub struct ApiClientConfig {
    /// Base URL of the server (e.g., "http://localhost:3000")
    pub base_url: String,
    /// Optional auth token for authenticated requests
    pub auth_token: Option<String>,
}

impl ApiClientConfig {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            auth_token: None,
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    pub fn url(&self, endpoint: &str) -> String {
        format!("{}{}", self.base_url, endpoint)
    }
}

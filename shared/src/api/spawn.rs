//! Agent-created sessions: persisted approvals and bounded child lineage.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::LaunchSpec;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnApprovalMode {
    #[default]
    FirstSpawn,
    Auto,
    Off,
}

/// Only a browser-authenticated owner may change this policy. A child gets
/// its own default policy; parent trust is never inherited implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnPolicy {
    pub approval_mode: SpawnApprovalMode,
    pub max_children: u32,
    /// Root sessions have depth 1. The default permits direct children only.
    pub max_depth: u32,
    pub idle_timeout_seconds: u32,
    pub stop_on_parent_end: bool,
}

impl Default for SpawnPolicy {
    fn default() -> Self {
        Self {
            approval_mode: SpawnApprovalMode::FirstSpawn,
            max_children: 4,
            max_depth: 2,
            idle_timeout_seconds: 1800,
            stop_on_parent_end: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnPolicyResponse {
    pub policy: SpawnPolicy,
    pub first_spawn_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSessionRequest {
    /// Caller-generated idempotency key: retrying this request never spawns
    /// another child, including after approval or a lost HTTP response.
    pub request_id: Uuid,
    pub launch: LaunchSpec,
    #[serde(default)]
    pub launcher_id: Option<Uuid>,
    pub prompt: String,
    /// Omitted means ordinary directory detection at the child's next launch.
    #[serde(default)]
    pub plugins: Option<std::collections::BTreeMap<String, bool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnRequestState {
    Pending,
    Launching,
    Launched,
    Denied,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequestInfo {
    pub request_id: Uuid,
    pub parent_session_id: Uuid,
    pub launcher_id: Uuid,
    pub request: SpawnSessionRequest,
    pub state: SpawnRequestState,
    pub child_session_id: Option<Uuid>,
    pub approved_by: Option<Uuid>,
    pub created_at: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequestsResponse {
    pub requests: Vec<SpawnRequestInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnChildrenResponse {
    pub children: Vec<crate::SessionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnErrorResponse {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
}

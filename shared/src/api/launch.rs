//! Launcher endpoint request/response types.

use serde::{Deserialize, Serialize};

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

/// Response from GET /api/launchers/:launcher_id/directories?path=…
///
/// Envelope around the already-shared `DirectoryEntry` payload type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirectoryListingResponse {
    #[serde(default)]
    pub entries: Vec<crate::DirectoryEntry>,
    #[serde(default)]
    pub resolved_path: Option<String>,
}

/// Response from GET /api/launchers/:launcher_id/probe-agents.
///
/// Envelope around the already-shared `AgentInstall` payload type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeAgentsResponse {
    #[serde(default)]
    pub agents: Vec<crate::AgentInstall>,
}

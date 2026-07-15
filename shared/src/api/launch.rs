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
    /// When true, the launcher creates a git worktree from the repository that
    /// contains `working_directory` and runs the session inside the new
    /// worktree instead of `working_directory` itself. Requires
    /// `working_directory` to be inside a git repository. Additive/opt-in:
    /// older launchers ignore it via `#[serde(default)]`.
    #[serde(default)]
    pub create_worktree: bool,
    /// Optional branch name for the worktree. When omitted, the launcher
    /// derives a timestamped default (e.g. `session-20260715-143022`).
    #[serde(default)]
    pub worktree_branch: Option<String>,
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

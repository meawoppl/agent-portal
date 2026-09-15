//! Launcher endpoint request/response types.

use serde::{Deserialize, Serialize};

/// Reusable parameters that describe where and how an agent session starts.
///
/// `session_name` is distinct from a scheduled task's own label. Interactive
/// launch requests retain their legacy top-level `name` as a compatibility
/// alias while new callers use this unambiguous shared field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: crate::AgentType,
    #[serde(default, skip_serializing_if = "WorktreeMode::is_none")]
    pub worktree: WorktreeMode,
}

/// Where a launched session should run relative to its source repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WorktreeMode {
    /// Run directly in `working_directory`.
    #[default]
    None,
    /// Create/reuse the repository's ordinary `.worktrees/<branch>` checkout.
    Repo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// Create/reuse a launcher-owned checkout under
    /// `~/.agent-portal/worktrees`, eligible for guarded cleanup.
    Scratch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
}

impl WorktreeMode {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Repo { branch } | Self::Scratch { branch } => branch.as_deref(),
        }
    }
}

/// Request to launch a session via a launcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    #[serde(flatten)]
    pub launch: LaunchSpec,
    #[serde(default)]
    pub launcher_id: Option<uuid::Uuid>,
    /// Optional human-chosen session name. When present (non-empty), it becomes
    /// the session's display name in the dashboard/nav instead of the working
    /// directory's basename. When `create_worktree` is set, it also names the
    /// worktree branch (sanitized launcher-side). Additive/opt-in: older
    /// backends ignore it via `#[serde(default)]`.
    #[serde(default)]
    pub name: Option<String>,
    /// When true, the launcher creates a git worktree from the repository that
    /// contains `working_directory` and runs the session inside the new
    /// worktree instead of `working_directory` itself. Requires
    /// `working_directory` to be inside a git repository. Additive/opt-in:
    /// older launchers ignore it via `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub create_worktree: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkDirectoryMode {
    Worktree,
    Same,
    Other,
}

/// Request body for `POST /api/sessions/:id/fork`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkSessionRequest {
    pub name: String,
    pub directory_mode: ForkDirectoryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divergence_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_point_turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkSessionResponse {
    pub request_id: uuid::Uuid,
    pub session_id: uuid::Uuid,
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

/// Body of POST /api/launchers/:id/agent-login/start — which agent to sign in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAgentLoginRequest {
    pub agent_type: crate::AgentType,
}

/// Response from the start endpoint: the flow id to reference in follow-up
/// calls, plus what the user must act on and how the flow finishes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAgentLoginResponse {
    pub flow_id: uuid::Uuid,
    pub presentable: crate::LoginPresentable,
    pub interaction: crate::LoginInteraction,
}

/// Body of POST /api/launchers/:id/agent-login/:flow/code — the pasted code
/// (claude's `SubmitCode` interaction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitAgentLoginCodeRequest {
    pub code: String,
}

/// Response from POST /api/launchers/:id/agents/:agent/install — whether the
/// install command succeeded, plus the CLI's own output tail on failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallAgentResponse {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_launch_request_keeps_flat_wire_shape() {
        let request: LaunchRequest = serde_json::from_str(
            r#"{"working_directory":"/home/me/repo","agent_type":"codex","claude_args":[],"create_worktree":true}"#,
        )
        .unwrap();
        assert_eq!(request.launch.working_directory, "/home/me/repo");
        assert!(request.launch.session_name.is_none());
        assert_eq!(request.launch.worktree, WorktreeMode::None);
        assert!(request.create_worktree);
    }

    #[test]
    fn scratch_worktree_roundtrips_with_branch() {
        let mode = WorktreeMode::Scratch {
            branch: Some("sched-123".to_string()),
        };
        let value = serde_json::to_value(&mode).unwrap();
        assert_eq!(value["mode"], "scratch");
        assert_eq!(serde_json::from_value::<WorktreeMode>(value).unwrap(), mode);
    }
}

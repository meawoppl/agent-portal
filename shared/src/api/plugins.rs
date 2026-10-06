//! Portal plugin inventory API types.

use serde::{Deserialize, Serialize};

/// Response from `GET /api/plugins`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginInventoryResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub scanned: bool,
    #[serde(default)]
    pub injected_context: Vec<PluginContextEntry>,
    #[serde(default)]
    pub plugins: Vec<PortalPluginInfo>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// A locally installed Portal plugin plus session/path-specific discovery
/// hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortalPluginInfo {
    pub name: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub activation: Option<PluginActivation>,
    #[serde(default)]
    pub disabled_by_policy: bool,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub has_doctor: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub update_available: Option<bool>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub compatibility: Vec<String>,
    #[serde(default)]
    pub policy: PluginPolicy,
    #[serde(default)]
    pub prompts: Vec<PortalPluginSkillInfo>,
    #[serde(default)]
    pub surface: Option<PluginSurfaceStatus>,
    #[serde(default)]
    pub suggested: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_width_percent: Option<u16>,
    #[serde(default)]
    pub skills: Vec<PortalPluginSkillInfo>,
    #[serde(default)]
    pub commands: Vec<PortalPluginCommandInfo>,
    #[serde(default)]
    pub context_bytes: u64,
    #[serde(default)]
    pub estimated_tokens: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortalPluginSkillInfo {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub context_bytes: u64,
    #[serde(default)]
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortalPluginCommandInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub accepts_args: bool,
}

/// Host-local policy, keyed by canonical project directory and plugin name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginPolicy {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRequest {
    pub working_directory: Option<String>,
    pub session_id: Option<uuid::Uuid>,
    pub action: PluginAction,
}

/// Versioned, named actions. Callers cannot supply a shell command to execute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginAction {
    Inventory,
    Install {
        source: String,
    },
    Update {
        name: String,
    },
    Remove {
        name: String,
    },
    Doctor {
        name: String,
    },
    SetPolicy {
        name: String,
        policy: PluginPolicy,
    },
    Surface {
        name: String,
        action: PluginSurfaceAction,
    },
    RunCommand {
        name: String,
        command: String,
        expected_run: String,
        args: Vec<String>,
        approved: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSurfaceAction {
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginResponse {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub inventory: Option<PluginInventoryResponse>,
    #[serde(default)]
    pub surface: Option<PluginSurfaceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginSurfaceState {
    #[default]
    Stopped,
    Starting,
    Healthy,
    Unhealthy,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginSurfaceStatus {
    pub state: PluginSurfaceState,
    pub port: Option<u16>,
    pub health_path: Option<String>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub log_tail: Vec<String>,
    pub pid: Option<u32>,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginActivation {
    Detected,
    Policy,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginOverride {
    pub name: String,
    pub enabled: bool,
}

/// The launch-time snapshot, never inferred from a later discovery scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginContextEntry {
    pub plugin: String,
    pub name: String,
    pub path: String,
    pub kind: PluginContextKind,
    pub activation: PluginActivation,
    pub reason: String,
    pub context_bytes: u64,
    pub estimated_tokens: u64,
    pub text: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginContextKind {
    Skill,
    Prompt,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_approval_is_explicit_and_arguments_round_trip() {
        let request = PluginRequest {
            working_directory: Some("/project".into()),
            session_id: None,
            action: PluginAction::RunCommand {
                name: "tools".into(),
                command: "inspect".into(),
                expected_run: "inspect --safe".into(),
                args: vec!["a file; $(literal)".into()],
                approved: false,
            },
        };
        let wire = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<PluginRequest>(&wire).unwrap(),
            request
        );
        let missing_approval = wire.replace(",\"approved\":false", "");
        assert!(serde_json::from_str::<PluginRequest>(&missing_approval).is_err());
    }

    #[test]
    fn legacy_inventory_defaults_to_no_injection_claim() {
        let response: PluginInventoryResponse =
            serde_json::from_str(r#"{"plugins":[{"name":"tools","displayName":"Tools"}]}"#)
                .unwrap();
        assert!(response.injected_context.is_empty());
        assert_eq!(response.plugins[0].policy, PluginPolicy::Ask);
        assert!(response.plugins[0].activation.is_none());
    }
}

/// Durable Portal command audit card, independent of an agent's shell tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommandRecord {
    pub plugin: String,
    pub command: String,
    pub run: String,
    pub args: Vec<String>,
    pub launcher_id: uuid::Uuid,
    pub working_directory: String,
    pub requested_by: uuid::Uuid,
    pub status: String,
    pub exit_code: Option<i32>,
    pub output: String,
}

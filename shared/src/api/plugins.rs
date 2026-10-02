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
    pub plugins: Vec<PortalPluginInfo>,
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
}

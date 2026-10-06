//! Typed views of `agent-portal-plugin.toml` used by the launcher and inventory.
//!
//! Each consumer deliberately validates only the fields it uses. In particular,
//! skill injection accepts unnamed manifests and ignores runtime sections; the
//! inventory accepts commands without `run` and wider surface width values.
//! Keeping these views explicit preserves those compatibility boundaries while
//! sharing section definitions. Filesystem access and TOML parsing stay with the
//! callers; `Value` lets runtime capabilities retain their native TOML values.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "Value: Deserialize<'de>"))]
pub struct RuntimeManifest<Value> {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub install: InstallSection,
    #[serde(default)]
    pub surface: Option<SurfaceSection>,
    #[serde(default)]
    pub skills: Vec<SkillSection<PathBuf>>,
    #[serde(default)]
    pub prompts: Vec<PromptSection>,
    #[serde(default)]
    pub commands: Vec<CommandSection>,
    #[serde(default)]
    pub toolchains: Vec<ToolchainSection>,
    #[serde(default)]
    pub capabilities: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct InstallSection {
    #[serde(default)]
    pub setup: Option<String>,
    #[serde(default)]
    pub doctor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SurfaceSection {
    #[serde(default)]
    pub default_title: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub health_path: Option<String>,
    #[serde(default)]
    pub default_width_percent: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptSection {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommandSection {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub run: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolchainSection {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub home: Option<PathBuf>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub doctor: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(bound(deserialize = "P: Deserialize<'de>"))]
pub struct SkillSection<P> {
    pub name: String,
    pub path: P,
    #[serde(default)]
    pub agents: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct InventoryManifest {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub surface: Option<PluginSurface>,
    #[serde(default)]
    pub commands: Vec<ManifestCommand>,
    #[serde(default)]
    pub skills: Vec<SkillSection<String>>,
    #[serde(default)]
    pub detect: Vec<ManifestDetect>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PluginSurface {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub default_title: Option<String>,
    #[serde(default)]
    pub default_width_percent: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ManifestCommand {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ManifestDetect {
    pub name: String,
    #[serde(default)]
    pub any: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct SkillsManifest {
    #[serde(default)]
    pub skills: Vec<SkillSection<PathBuf>>,
}

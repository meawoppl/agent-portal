//! Read-only Portal plugin inventory and directory-specific suggestions.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use shared::api::{
    PluginInventoryResponse, PortalPluginCommandInfo, PortalPluginInfo, PortalPluginSkillInfo,
};
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::session_access::verify_session_reader;
use crate::AppState;

const MANIFEST_FILE: &str = "agent-portal-plugin.toml";
const MAX_SCAN_DEPTH: usize = 4;
const MAX_SCAN_ENTRIES: usize = 4096;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInventoryQuery {
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub working_directory: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PluginManifest {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    surface: Option<PluginSurface>,
    #[serde(default)]
    commands: Vec<ManifestCommand>,
    #[serde(default)]
    skills: Vec<ManifestSkill>,
    #[serde(default)]
    detect: Vec<ManifestDetect>,
}

#[derive(Debug, Default, Deserialize)]
struct PluginSurface {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    default_title: Option<String>,
    #[serde(default)]
    default_width_percent: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestCommand {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestSkill {
    name: String,
    path: String,
    #[serde(default)]
    agents: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestDetect {
    name: String,
    #[serde(default)]
    any: Vec<String>,
}

/// GET /api/plugins
pub async fn list_plugins(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Query(query): Query<PluginInventoryQuery>,
) -> Result<Json<PluginInventoryResponse>, AppError> {
    let working_directory = if let Some(session_id) = query.session_id {
        let mut conn = app_state.conn()?;
        let session = verify_session_reader(&mut conn, session_id, current_user_id)?;
        Some(session.working_directory)
    } else {
        query
            .working_directory
            .and_then(|path| shared::strings::trimmed_non_blank(Some(&path)).map(str::to_string))
    };

    Ok(Json(discover_plugins(working_directory)))
}

fn discover_plugins(working_directory: Option<String>) -> PluginInventoryResponse {
    let scan_root = working_directory.as_deref().and_then(expand_tilde);
    let scanned = scan_root.as_ref().is_some_and(|path| path.exists());
    let mut plugins = Vec::new();

    for plugin_dir in manifest_dirs() {
        let Some(plugin) = read_plugin(&plugin_dir, scan_root.as_deref()) else {
            continue;
        };
        plugins.push(plugin);
    }

    plugins.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| b.suggested.cmp(&a.suggested))
            .then_with(|| a.display_name.cmp(&b.display_name))
    });

    PluginInventoryResponse {
        working_directory,
        scanned,
        plugins,
        ..Default::default()
    }
}

fn read_plugin(plugin_dir: &Path, scan_root: Option<&Path>) -> Option<PortalPluginInfo> {
    let manifest_path = plugin_dir.join(MANIFEST_FILE);
    let raw = match fs::read_to_string(&manifest_path) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(
                "failed to read plugin manifest {}: {err}",
                manifest_path.display()
            );
            return None;
        }
    };
    let manifest: PluginManifest = match toml::from_str(&raw) {
        Ok(manifest) => manifest,
        Err(err) => {
            tracing::warn!(
                "failed to parse plugin manifest {}: {err}",
                manifest_path.display()
            );
            return None;
        }
    };
    if !shared::strings::is_non_blank(&manifest.name) {
        return None;
    }

    let mut warnings = Vec::new();
    let skills = manifest
        .skills
        .iter()
        .map(|skill| skill_info(plugin_dir, skill, &mut warnings))
        .collect::<Vec<_>>();
    let context_bytes = skills.iter().map(|skill| skill.context_bytes).sum::<u64>();
    let estimated_tokens = skills
        .iter()
        .map(|skill| skill.estimated_tokens)
        .sum::<u64>();

    let reason = scan_root.and_then(|root| detect_reason(root, &manifest.detect));
    let suggested = reason.is_some();
    let surface = manifest.surface.unwrap_or_default();
    let display_name = manifest
        .display_name
        .filter(|value| shared::strings::is_non_blank(value))
        .unwrap_or_else(|| manifest.name.clone());

    Some(PortalPluginInfo {
        name: manifest.name,
        display_name,
        description: manifest.description,
        installed: true,
        suggested,
        active: suggested,
        reason,
        source: Some(plugin_dir.display().to_string()),
        surface_kind: surface.kind,
        surface_title: surface.default_title,
        surface_width_percent: surface.default_width_percent,
        skills,
        commands: manifest
            .commands
            .into_iter()
            .filter(|command| shared::strings::is_non_blank(&command.name))
            .map(|command| PortalPluginCommandInfo {
                name: command.name,
                description: command.description,
                ..Default::default()
            })
            .collect(),
        context_bytes,
        estimated_tokens,
        warnings,
        ..Default::default()
    })
}

fn skill_info(
    plugin_dir: &Path,
    skill: &ManifestSkill,
    warnings: &mut Vec<String>,
) -> PortalPluginSkillInfo {
    let path = plugin_dir.join(&skill.path);
    let (description, context_bytes) = match fs::read_to_string(&path) {
        Ok(contents) => (skill_description(&contents), contents.len() as u64),
        Err(err) => {
            warnings.push(format!("Could not read skill {}: {err}", skill.path));
            (None, 0)
        }
    };
    PortalPluginSkillInfo {
        name: skill.name.clone(),
        path: skill.path.clone(),
        agents: skill.agents.clone(),
        description,
        context_bytes,
        estimated_tokens: estimate_tokens(context_bytes),
    }
}

fn skill_description(contents: &str) -> Option<String> {
    contents
        .lines()
        .map(str::trim)
        .find(|line| shared::strings::is_non_empty(line))
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .filter(|line| shared::strings::is_non_empty(line))
}

fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

fn detect_reason(root: &Path, detects: &[ManifestDetect]) -> Option<String> {
    for detect in detects {
        if detect.any.is_empty() {
            continue;
        }
        if directory_matches(root, &detect.any) {
            return Some(format!(
                "Detected {} files ({})",
                detect.name,
                detect.any.join(", ")
            ));
        }
    }
    None
}

fn directory_matches(root: &Path, patterns: &[String]) -> bool {
    if path_matches(root, patterns) {
        return true;
    }
    if !root.is_dir() {
        return false;
    }

    let mut seen = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_SCAN_ENTRIES {
                return false;
            }
            let path = entry.path();
            if path_matches(&path, patterns) {
                return true;
            }
            if depth < MAX_SCAN_DEPTH && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with('.') {
                    stack.push((path, depth + 1));
                }
            }
        }
    }
    false
}

fn path_matches(path: &Path, patterns: &[String]) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    patterns
        .iter()
        .any(|pattern| file_name_matches(file_name, pattern))
}

fn file_name_matches(file_name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return file_name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return file_name.starts_with(prefix);
    }
    file_name == pattern
}

fn manifest_dirs() -> Vec<PathBuf> {
    let mut roots = BTreeSet::<PathBuf>::new();
    for var in [
        "PORTAL_PLUGIN_DIRS",
        "AGENT_PORTAL_PLUGIN_DIRS",
        "AGENT_PORTAL_PLUGIN_ROOT",
    ] {
        if let Some(value) = env::var_os(var) {
            roots.extend(env::split_paths(&value));
        }
    }
    if let Some(home) = env::var_os("HOME") {
        roots.insert(PathBuf::from(home).join("agent-portal-plugins"));
    }

    let mut dirs = BTreeSet::<PathBuf>::new();
    for root in roots {
        if root.join(MANIFEST_FILE).exists() {
            dirs.insert(root);
            continue;
        }
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') || !path.is_dir() || !path.join(MANIFEST_FILE).exists() {
                continue;
            }
            dirs.insert(path);
        }
    }
    dirs.into_iter().collect()
}

fn expand_tilde(path: &str) -> Option<PathBuf> {
    if path == "~" {
        return env::var_os("HOME").map(PathBuf::from);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return env::var_os("HOME").map(|home| PathBuf::from(home).join(rest));
    }
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matching_handles_manifest_detect_patterns() {
        assert!(file_name_matches("board.kicad_pcb", "*.kicad_pcb"));
        assert!(file_name_matches(
            ".verilog-workbench.json",
            ".verilog-workbench.json"
        ));
        assert!(file_name_matches("module_test.sv", "*.sv"));
        assert!(!file_name_matches("notes.txt", "*.sv"));
    }

    #[test]
    fn token_estimate_rounds_up() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
    }
}

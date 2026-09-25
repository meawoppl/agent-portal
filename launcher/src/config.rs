use serde::{Deserialize, Serialize};
use shared::AgentType;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct LauncherConfig {
    pub backend_url: Option<String>,
    pub auth_token: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub sessions: Vec<ExpectedSession>,
    #[serde(default)]
    pub plugins: std::collections::BTreeMap<String, InstalledPlugin>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstalledPlugin {
    pub path: String,
    pub source: String,
    #[serde(default)]
    pub source_subdir: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub installed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, Default)]
struct PluginManifest {
    #[serde(default)]
    skills: Vec<SkillSection>,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillSection {
    name: String,
    path: PathBuf,
    #[serde(default)]
    agents: Vec<String>,
}

#[derive(Debug, Default)]
pub struct PluginSkillContext {
    pub claude_plugin_dirs: Vec<PathBuf>,
    pub reminder: Option<String>,
    pub cleanup_dir: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExpectedSession {
    pub working_directory: String,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

fn config_dir() -> PathBuf {
    session_lib::paths::config_dir()
}

fn config_path() -> PathBuf {
    config_dir().join("launcher.json")
}

/// Human-readable location of the launcher config, for user-facing messages
/// (e.g. telling `login` where it just saved credentials).
pub fn config_path_display() -> String {
    config_path().display().to_string()
}

pub fn load_config() -> LauncherConfig {
    load_config_at(&config_path())
}

/// Load a [`LauncherConfig`] from an explicit path. Split out from
/// [`load_config`] so the read-modify-write helpers can be tested against a
/// temp path (mirrors the [`read_or_create_id`] seam).
fn load_config_at(path: &Path) -> LauncherConfig {
    match std::fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(config) => {
                tracing::info!("Loaded config from {}", path.display());
                config
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", path.display(), e);
                LauncherConfig::default()
            }
        },
        Err(_) => LauncherConfig::default(),
    }
}

fn save_config(config: &LauncherConfig) -> anyhow::Result<()> {
    let path = config_path();
    let contents = serde_json::to_string_pretty(config)?;
    write_config_atomic(&path, &contents)?;
    tracing::debug!("Saved config to {}", path.display());
    Ok(())
}

pub fn plugin_root() -> PathBuf {
    std::env::var_os("AGENT_PORTAL_PLUGIN_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("agent-portal-plugins")
        })
}

pub fn save_installed_plugin(name: &str, plugin: InstalledPlugin) -> anyhow::Result<()> {
    let mut config = load_config();
    config.plugins.insert(name.to_string(), plugin);
    save_config(&config)
}

pub fn remove_installed_plugin(name: &str) -> anyhow::Result<Option<InstalledPlugin>> {
    let mut config = load_config();
    let removed = config.plugins.remove(name);
    if removed.is_some() {
        save_config(&config)?;
    }
    Ok(removed)
}

pub fn set_plugin_enabled(name: &str, enabled: bool) -> anyhow::Result<()> {
    let mut config = load_config();
    let Some(plugin) = config.plugins.get_mut(name) else {
        anyhow::bail!("plugin `{name}` is not installed");
    };
    plugin.enabled = enabled;
    save_config(&config)
}

pub fn plugin_skill_context(agent_type: AgentType, session_id: Uuid) -> PluginSkillContext {
    plugin_skill_context_from_config(
        &load_config(),
        &config_dir()
            .join("plugin-skill-roots")
            .join(session_id.to_string()),
        agent_type,
    )
}

fn plugin_skill_context_from_config(
    config: &LauncherConfig,
    generated_root: &Path,
    agent_type: AgentType,
) -> PluginSkillContext {
    let mut context = PluginSkillContext::default();
    let mut lines = Vec::new();
    if agent_type == AgentType::Claude {
        let _ = std::fs::remove_dir_all(generated_root);
    }

    for (plugin_name, plugin) in &config.plugins {
        if !plugin.enabled {
            continue;
        }
        let root = PathBuf::from(&plugin.path);
        let Some(manifest) = read_plugin_manifest(&root) else {
            continue;
        };
        let claude_root = generated_root.join(plugin_name);
        let mut has_claude_skill = false;
        for skill in manifest.skills {
            let path = root.join(&skill.path);
            if !path.is_file() {
                continue;
            }
            let metadata = read_skill_metadata(&path);
            let skill_name = metadata.name.unwrap_or_else(|| skill.name.clone());
            let description = metadata.description;
            if agent_type == AgentType::Claude
                && skill_applies_to(&skill, "claude")
                && materialize_claude_skill(&claude_root, &skill_name, &path).is_ok()
            {
                has_claude_skill = true;
            }
            let reminder_agent = match agent_type {
                AgentType::Codex => Some("codex"),
                AgentType::Muse => Some("muse"),
                AgentType::Claude => None,
            };
            if reminder_agent.is_some_and(|agent| skill_applies_to(&skill, agent)) {
                let description = description
                    .as_deref()
                    .map(|description| format!(" - {description}"))
                    .unwrap_or_default();
                lines.push(format!(
                    "- `{plugin_name}:{skill_name}`{description}: {}",
                    path.display()
                ));
            }
        }
        if has_claude_skill {
            context.claude_plugin_dirs.push(claude_root);
        }
    }
    if !context.claude_plugin_dirs.is_empty() {
        context.cleanup_dir = Some(generated_root.to_path_buf());
    }
    if !lines.is_empty() {
        lines.sort();
        context.reminder = Some(format!(
            "## Plugin Skills\n\nInstalled Agent Portal plugins provide these skills. When a user task matches one, read its `SKILL.md` completely before acting, then follow its instructions:\n\n{}",
            lines.join("\n")
        ));
    }
    context
}

fn read_plugin_manifest(root: &Path) -> Option<PluginManifest> {
    let contents = std::fs::read_to_string(root.join("agent-portal-plugin.toml")).ok()?;
    toml::from_str(&contents).ok()
}

fn skill_applies_to(skill: &SkillSection, agent: &str) -> bool {
    skill.agents.is_empty()
        || skill
            .agents
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(agent))
}

#[derive(Default)]
struct SkillMetadata {
    name: Option<String>,
    description: Option<String>,
}

fn read_skill_metadata(path: &Path) -> SkillMetadata {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return SkillMetadata::default();
    };
    let mut lines = contents.lines();
    if lines.next() != Some("---") {
        return SkillMetadata::default();
    }
    let mut metadata = SkillMetadata::default();
    for line in lines {
        if line == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        match key.trim() {
            "name" if !value.is_empty() => metadata.name = Some(value),
            "description" if !value.is_empty() => metadata.description = Some(value),
            _ => {}
        }
    }
    metadata
}

fn materialize_claude_skill(
    claude_root: &Path,
    skill_name: &str,
    skill_path: &Path,
) -> anyhow::Result<()> {
    let skill_dir = claude_root.join("skills").join(skill_name);
    if let Some(parent) = skill_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let source_dir = skill_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("skill path has no parent: {}", skill_path.display()))?;
    symlink_or_copy_dir(source_dir, &skill_dir)
}

fn symlink_or_copy_dir(source: &Path, dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, dest)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        copy_dir_recursive(source, dest)
    }
}

#[cfg(not(unix))]
fn copy_dir_recursive(source: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &dest_path)?;
        } else {
            std::fs::copy(&source_path, &dest_path)?;
        }
    }
    Ok(())
}

fn write_config_atomic(path: &Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write-temp + rename so the file is never observed half-written: the
    // parked-recovery path re-reads launcher.json on every connection attempt
    // (#1237), so a torn write during `agent-portal login` must not be
    // readable as a corrupt config.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_auth_token(token: &str) -> anyhow::Result<()> {
    let mut config = load_config();
    config.auth_token = Some(token.to_string());
    save_config(&config)?;
    tracing::info!("Saved auth token to {}", config_path().display());
    Ok(())
}

/// Persist `backend_url` and `auth_token` together in a single load/save cycle.
///
/// A token is only valid against the server that minted it, so the two MUST be
/// written as a pair: `agent-portal login --backend-url wss://newhost` that
/// stored only the token would leave `backend_url` pointing at the old server,
/// and the next plain run (or the installed launchd/systemd unit, whose
/// command line carries no `--backend-url`) would connect to the old host with
/// a token it won't accept. Callers pass the *resolved* URL so a flag-supplied
/// host sticks for subsequent runs. (`save_auth_token` remains for same-server
/// token refreshes, where `backend_url` must not change.)
pub fn save_credentials(backend_url: &str, token: &str) -> anyhow::Result<()> {
    save_credentials_at(&config_path(), backend_url, token)
}

/// [`save_credentials`] against an explicit path, so the atomic round-trip can
/// be unit-tested without touching the real config location.
fn save_credentials_at(path: &Path, backend_url: &str, token: &str) -> anyhow::Result<()> {
    let mut config = load_config_at(path);
    config.backend_url = Some(backend_url.to_string());
    config.auth_token = Some(token.to_string());
    let contents = serde_json::to_string_pretty(&config)?;
    write_config_atomic(path, &contents)?;
    Ok(())
}

fn launcher_id_path() -> PathBuf {
    config_dir().join("launcher_id")
}

/// A stable launcher identity that survives restarts.
///
/// Reconcile selects a launcher's desired sessions by `sessions.launcher_id`,
/// so minting a fresh random id on every boot stranded a restarted launcher's
/// own sessions — none matched the new id until each proxy happened to
/// reconnect on its own. Persisting the id (creating one on first run) lets a
/// restarted launcher reclaim and relaunch its prior sessions.
pub fn persistent_launcher_id() -> Uuid {
    read_or_create_id(&launcher_id_path())
}

/// Read a UUID from `path`, or generate one and write it. Split out from
/// [`persistent_launcher_id`] so it can be tested against a temp path.
fn read_or_create_id(path: &Path) -> Uuid {
    if let Ok(contents) = std::fs::read_to_string(path) {
        if let Ok(id) = Uuid::parse_str(contents.trim()) {
            return id;
        }
    }
    let id = Uuid::new_v4();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, id.to_string()) {
        tracing::warn!(
            "Failed to persist launcher id to {} ({}); using ephemeral id {} for this run",
            path.display(),
            e,
            id
        );
    }
    id
}

pub fn clear_sessions() -> anyhow::Result<()> {
    let mut config = load_config();
    if !config.sessions.is_empty() {
        config.sessions.clear();
        save_config(&config)?;
        tracing::info!("Cleared launcher-local expected sessions from config");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_id_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher_id");
        // Created on first call, identical on the second (survives "restart").
        let first = read_or_create_id(&path);
        assert!(path.exists());
        let second = read_or_create_id(&path);
        assert_eq!(first, second);
    }

    #[test]
    fn launcher_id_regenerates_on_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher_id");
        std::fs::write(&path, "not-a-uuid").unwrap();
        let id = read_or_create_id(&path);
        // A fresh valid id was written back over the garbage.
        assert_eq!(
            Uuid::parse_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap(),
            id
        );
    }

    #[test]
    fn save_credentials_persists_both_fields_together() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");

        // Fresh file: both fields land together.
        save_credentials_at(&path, "wss://newhost.example", "tok_new").unwrap();
        let config = load_config_at(&path);
        assert_eq!(config.backend_url.as_deref(), Some("wss://newhost.example"));
        assert_eq!(config.auth_token.as_deref(), Some("tok_new"));

        // Switching servers updates the URL *and* the token in one cycle — the
        // stale-URL-with-new-token drift this bug was about must not survive.
        save_credentials_at(&path, "wss://other.example", "tok_other").unwrap();
        let switched = load_config_at(&path);
        assert_eq!(switched.backend_url.as_deref(), Some("wss://other.example"));
        assert_eq!(switched.auth_token.as_deref(), Some("tok_other"));
    }

    #[test]
    fn save_credentials_preserves_unrelated_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        std::fs::write(
            &path,
            r#"{"backend_url":"wss://old","auth_token":"old","name":"my-launcher"}"#,
        )
        .unwrap();

        save_credentials_at(&path, "wss://new", "tok_new").unwrap();

        let config = load_config_at(&path);
        assert_eq!(config.backend_url.as_deref(), Some("wss://new"));
        assert_eq!(config.auth_token.as_deref(), Some("tok_new"));
        // The load/save cycle must not drop fields it doesn't touch.
        assert_eq!(config.name.as_deref(), Some("my-launcher"));
    }

    #[test]
    fn parse_full_config() {
        let json = r#"{
            "backend_url": "wss://example.com",
            "auth_token": "tok_abc123",
            "name": "my-launcher"
        }"#;
        let config: LauncherConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.backend_url.unwrap(), "wss://example.com");
        assert_eq!(config.auth_token.unwrap(), "tok_abc123");
        assert_eq!(config.name.unwrap(), "my-launcher");
        assert!(config.sessions.is_empty());
    }

    #[test]
    fn parse_empty_config() {
        let config: LauncherConfig = serde_json::from_str("{}").unwrap();
        assert!(config.backend_url.is_none());
        assert!(config.auth_token.is_none());
        assert!(config.name.is_none());
        assert!(config.sessions.is_empty());
    }

    #[test]
    fn parse_partial_config() {
        let json = r#"{ "auth_token": "secret" }"#;
        let config: LauncherConfig = serde_json::from_str(json).unwrap();
        assert!(config.backend_url.is_none());
        assert_eq!(config.auth_token.unwrap(), "secret");
    }

    #[test]
    fn config_path_is_absolute() {
        let path = config_path();
        assert!(path.is_absolute());
    }

    #[test]
    fn write_config_atomic_replaces_file_and_removes_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        std::fs::write(&path, "old").unwrap();

        write_config_atomic(&path, r#"{"auth_token":"new"}"#).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"auth_token":"new"}"#
        );
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn roundtrip_config_serialization() {
        let config = LauncherConfig {
            backend_url: Some("wss://test.com".to_string()),
            auth_token: Some("tok_test".to_string()),
            name: Some("test-launcher".to_string()),
            sessions: vec![ExpectedSession {
                working_directory: "/home/user/project".to_string(),
                session_name: Some("my-session".to_string()),
                agent_type: AgentType::Claude,
                claude_args: vec!["--verbose".to_string()],
                session_id: None,
            }],
            plugins: std::collections::BTreeMap::new(),
        };
        let serialized = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: LauncherConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.backend_url, config.backend_url);
        assert_eq!(deserialized.auth_token, config.auth_token);
        assert_eq!(deserialized.name, config.name);
        assert_eq!(deserialized.sessions.len(), 1);
        assert_eq!(
            deserialized.sessions[0].working_directory,
            "/home/user/project"
        );
    }

    #[test]
    fn parse_config_with_sessions() {
        let json = r#"{
            "backend_url": "wss://example.com",
            "auth_token": "tok_abc",
            "sessions": [
                {
                    "working_directory": "/home/user/project-a",
                    "session_name": "project-a"
                },
                {
                    "working_directory": "/home/user/project-b",
                    "agent_type": "codex",
                    "claude_args": ["--model", "opus"]
                }
            ]
        }"#;
        let config: LauncherConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.sessions.len(), 2);
        assert!(config.plugins.is_empty());

        assert_eq!(config.sessions[0].working_directory, "/home/user/project-a");
        assert_eq!(
            config.sessions[0].session_name.as_deref(),
            Some("project-a")
        );
        assert_eq!(config.sessions[0].agent_type, AgentType::Claude);
        assert!(config.sessions[0].claude_args.is_empty());
        assert!(config.sessions[0].session_id.is_none());

        assert_eq!(config.sessions[1].working_directory, "/home/user/project-b");
        assert!(config.sessions[1].session_name.is_none());
        assert_eq!(config.sessions[1].agent_type, AgentType::Codex);
        assert_eq!(config.sessions[1].claude_args, vec!["--model", "opus"]);
        assert!(config.sessions[1].session_id.is_none());
    }

    #[test]
    fn parse_config_with_session_id() {
        let json = r#"{
            "backend_url": "wss://example.com",
            "auth_token": "tok_abc",
            "sessions": [{
                "working_directory": "/home/user/project-a",
                "session_id": "550e8400-e29b-41d4-a716-446655440000"
            }]
        }"#;
        let config: LauncherConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.sessions.len(), 1);
        assert_eq!(
            config.sessions[0].session_id,
            Some(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap())
        );
    }

    #[test]
    fn plugin_skill_context_uses_manifest_frontmatter_and_generated_claude_root() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_root = dir.path().join("plugins").join("kicad-pcb");
        let skill_dir = plugin_root.join("nested").join("layout");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            plugin_root.join("agent-portal-plugin.toml"),
            r#"
[[skills]]
name = "manifest-layout"
path = "nested/layout/SKILL.md"
agents = ["claude", "codex"]
"#,
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: kicad-layout
description: Route KiCad PCB layouts.
---

# Layout
"#,
        )
        .unwrap();

        let mut plugins = std::collections::BTreeMap::new();
        plugins.insert(
            "kicad-pcb".to_string(),
            InstalledPlugin {
                path: plugin_root.display().to_string(),
                source: "local".to_string(),
                source_subdir: None,
                reference: None,
                enabled: true,
                installed_at: chrono::Utc::now(),
            },
        );
        let config = LauncherConfig {
            plugins,
            ..Default::default()
        };
        let generated_root = dir.path().join("generated");

        let context = plugin_skill_context_from_config(&config, &generated_root, AgentType::Claude);

        assert_eq!(
            context.claude_plugin_dirs,
            vec![generated_root.join("kicad-pcb")]
        );
        assert_eq!(
            context.cleanup_dir.as_deref(),
            Some(generated_root.as_path())
        );
        assert!(generated_root
            .join("kicad-pcb")
            .join("skills")
            .join("kicad-layout")
            .join("SKILL.md")
            .is_file());
        assert!(context.reminder.is_none());

        let context = plugin_skill_context_from_config(&config, &generated_root, AgentType::Codex);
        assert!(context.claude_plugin_dirs.is_empty());
        assert!(context.cleanup_dir.is_none());
        let reminder = context.reminder.unwrap();
        assert!(reminder.contains("`kicad-pcb:kicad-layout`"));
        assert!(reminder.contains("Route KiCad PCB layouts."));
        assert!(reminder.contains("nested/layout/SKILL.md"));

        let context = plugin_skill_context_from_config(&config, &generated_root, AgentType::Muse);
        assert!(context.reminder.is_none());
    }

    #[test]
    fn roundtrip_config_with_session_id() {
        let sid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let config = LauncherConfig {
            backend_url: None,
            auth_token: None,
            name: None,
            sessions: vec![ExpectedSession {
                working_directory: "/home/user/project".to_string(),
                session_name: None,
                agent_type: AgentType::Claude,
                claude_args: vec![],
                session_id: Some(sid),
            }],
            plugins: std::collections::BTreeMap::new(),
        };
        let serialized = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: LauncherConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.sessions[0].session_id, Some(sid));
    }
}

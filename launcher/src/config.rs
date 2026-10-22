use serde::{Deserialize, Serialize};
use shared::api::{PluginActivation, PluginContextEntry, PluginContextKind};
use shared::plugin_manifest::{SkillSection as ManifestSkill, SkillsManifest as PluginManifest};
use shared::AgentType;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct LauncherConfig {
    /// Optional per-session cgroup v2 memory limits in bytes. Only applied on
    /// Linux hosts with delegated memory control; new session launches read the current values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_memory: Option<SessionMemoryLimits>,
    pub backend_url: Option<String>,
    pub auth_token: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub sessions: Vec<ExpectedSession>,
    #[serde(default)]
    pub plugins: std::collections::BTreeMap<String, InstalledPlugin>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SessionMemoryLimits {
    pub high_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
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
    /// Full git commit of the installed checkout, when the source was a git
    /// repository. Compared against the remote to report available updates.
    #[serde(default)]
    pub revision: Option<String>,
}

type SkillSection = ManifestSkill<PathBuf>;

#[derive(Debug, Default)]
pub struct PluginSkillContext {
    pub claude_plugin_dirs: Vec<PathBuf>,
    pub reminder: Option<String>,
    pub cleanup_dir: Option<PathBuf>,
    /// What this launch handed the agent, recorded so the session can show it.
    pub injected: Vec<PluginContextEntry>,
}

/// Text kept per injected file for inspection; the agent reads the file.
const INJECTED_TEXT_LIMIT: usize = 64 * 1024;

/// The source file's size and a bounded preview of it, read without loading
/// more than [`INJECTED_TEXT_LIMIT`].
fn source_preview(path: &Path) -> (u64, String) {
    use std::io::Read;
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|file| {
        file.take(INJECTED_TEXT_LIMIT as u64)
            .read_to_end(&mut bytes)
    });
    if read.is_err() {
        return (size, String::new());
    }
    let mut preview = String::from_utf8_lossy(&bytes).into_owned();
    if size > INJECTED_TEXT_LIMIT as u64 {
        preview.push_str("\n…");
    }
    (size, preview)
}

/// A skill made available at launch. The agent reads its body on demand, so
/// only `listed` (its catalog or reminder line) entered the context; the
/// source is recorded as its size and a preview, not as injected text.
fn available_skill(plugin: &str, name: &str, path: &Path, listed: &str) -> PluginContextEntry {
    let (size, preview) = source_preview(path);
    PluginContextEntry {
        plugin: plugin.to_string(),
        name: name.to_string(),
        path: path.display().to_string(),
        kind: PluginContextKind::Skill,
        // Filled from the plugin's resolution by the caller.
        activation: PluginActivation::Detected,
        reason: String::new(),
        context_bytes: size,
        estimated_tokens: (listed.len() as u64).div_ceil(4),
        text: preview,
    }
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

/// The config for a read that can't harm it: a file that doesn't parse reads
/// as the defaults, with a warning. Startup and every write use
/// [`load_config_checked`] instead.
pub fn load_config() -> LauncherConfig {
    load_config_at(&config_path())
}

/// The config at startup: a missing file is a first run (the defaults), but
/// one that exists and doesn't parse stops the launcher. Running on defaults
/// would sign in to the default server, and the first save would erase every
/// setting and plugin in the file.
pub fn load_config_checked() -> anyhow::Result<LauncherConfig> {
    let path = config_path();
    let config = load_config_for_write_at(&path)?;
    if path.exists() {
        tracing::info!("Loaded config from {}", path.display());
    } else {
        tracing::info!("No config at {}; using defaults", path.display());
    }
    Ok(config)
}

/// Session limits must never disappear because a running daemon's config was
/// edited incorrectly. Missing files retain the first-install defaults.
pub fn load_session_memory() -> anyhow::Result<SessionMemoryLimits> {
    load_session_memory_at(&config_path())
}

fn load_session_memory_at(path: &Path) -> anyhow::Result<SessionMemoryLimits> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SessionMemoryLimits::default())
        }
        Err(error) => return Err(error.into()),
    };
    let config: LauncherConfig = serde_json::from_str(&contents)?;
    Ok(config.session_memory.unwrap_or_default())
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

/// Read the config in order to change it. Unlike [`load_config`], a file that
/// exists but can't be read or parsed is an error: saving over it would erase
/// every other install, the auth token, and the rest of its settings.
fn load_config_for_write_at(path: &Path) -> anyhow::Result<LauncherConfig> {
    use anyhow::Context;
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents)
            .with_context(|| format!("{} is not valid; fix or move it aside", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LauncherConfig::default()),
        Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
    }
}

/// Apply `change` to the config at `path`, saving only when it reports a
/// change. Plugin installs, removals and toggles go through here.
fn update_config_at<T>(
    path: &Path,
    change: impl FnOnce(&mut LauncherConfig) -> anyhow::Result<(T, bool)>,
) -> anyhow::Result<T> {
    let mut config = load_config_for_write_at(path)?;
    let (result, changed) = change(&mut config)?;
    if changed {
        write_config_atomic(path, &serde_json::to_string_pretty(&config)?)?;
    }
    Ok(result)
}

pub fn save_installed_plugin(name: &str, plugin: InstalledPlugin) -> anyhow::Result<()> {
    update_config_at(&config_path(), |config| {
        config.plugins.insert(name.to_string(), plugin);
        Ok(((), true))
    })
}

pub fn remove_installed_plugin(name: &str) -> anyhow::Result<Option<InstalledPlugin>> {
    update_config_at(&config_path(), |config| {
        let removed = config.plugins.remove(name);
        let changed = removed.is_some();
        Ok((removed, changed))
    })
}

pub fn set_plugin_enabled(name: &str, enabled: bool) -> anyhow::Result<()> {
    update_config_at(&config_path(), |config| {
        let Some(plugin) = config.plugins.get_mut(name) else {
            anyhow::bail!("plugin `{name}` is not installed");
        };
        plugin.enabled = enabled;
        Ok(((), true))
    })
}

pub fn plugin_skill_context(
    agent_type: AgentType,
    session_id: Uuid,
    working_directory: &Path,
) -> PluginSkillContext {
    let mut config = load_config();
    let resolutions =
        match crate::session_plugins::apply_selection(&mut config, session_id, working_directory) {
            Ok(resolutions) => resolutions,
            Err(error) => {
                // Never silently re-enable excluded skills if the selection cannot be read.
                tracing::error!("Could not read session plugin selection: {error:#}");
                crate::session_plugins::record_injected(session_id, &[]);
                return PluginSkillContext::default();
            }
        };
    let mut context = plugin_skill_context_from_config(
        &config,
        &config_dir()
            .join("plugin-skill-roots")
            .join(session_id.to_string()),
        agent_type,
    );
    annotate_activation(&mut context.injected, &resolutions);
    crate::session_plugins::record_injected(session_id, &context.injected);
    context
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
                // Claude's skill catalog carries the name and description.
                let listed = format!("{skill_name}: {}", description.as_deref().unwrap_or(""));
                context
                    .injected
                    .push(available_skill(plugin_name, &skill_name, &path, &listed));
            }
            let reminder_agent = match agent_type {
                AgentType::Codex => Some("codex"),
                AgentType::Muse => Some("muse"),
                AgentType::Antigravity => Some("antigravity"),
                AgentType::Claude => None,
            };
            if reminder_agent.is_some_and(|agent| skill_applies_to(&skill, agent)) {
                let description = description
                    .as_deref()
                    .map(|description| format!(" - {description}"))
                    .unwrap_or_default();
                let line = format!(
                    "- `{plugin_name}:{skill_name}`{description}: {}",
                    path.display()
                );
                context
                    .injected
                    .push(available_skill(plugin_name, &skill_name, &path, &line));
                lines.push(line);
            }
        }
        if has_claude_skill {
            context.claude_plugin_dirs.push(claude_root);
        }
    }
    if !context.claude_plugin_dirs.is_empty() {
        context.cleanup_dir = Some(generated_root.to_path_buf());
    }
    let mut sections = Vec::new();
    if !lines.is_empty() {
        lines.sort();
        sections.push(format!(
            "## Plugin Skills\n\nInstalled Agent Portal plugins provide these skills. When a user task matches one, read its `SKILL.md` completely before acting, then follow its instructions:\n\n{}",
            lines.join("\n")
        ));
    }
    let (prompts, prompt_entries) = plugin_prompts(config);
    context.injected.extend(prompt_entries);
    sections.extend(prompts);
    if !sections.is_empty() {
        context.reminder = Some(sections.join("\n\n"));
    }
    context
}

/// Enabled plugins' `[[prompts]]`, as one reminder section plus their record.
/// Every agent gets them as text; there is no native loader for prompts.
fn plugin_prompts(config: &LauncherConfig) -> (Option<String>, Vec<PluginContextEntry>) {
    let mut parts = Vec::new();
    let mut entries = Vec::new();
    for (plugin_name, plugin) in config.plugins.iter().filter(|(_, p)| p.enabled) {
        let root = PathBuf::from(&plugin.path);
        let Ok(manifest) = crate::plugin::load_manifest(&root) else {
            continue;
        };
        for prompt in manifest.prompts {
            let path = root.join(&prompt.path);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            parts.push(format!(
                "### {plugin_name}: {}\n\n{}",
                prompt.name,
                text.trim()
            ));
            let mut entry = available_skill(plugin_name, &prompt.name, &path, &text);
            entry.kind = PluginContextKind::Prompt;
            entries.push(entry);
        }
    }
    let section = (!parts.is_empty()).then(|| {
        format!(
            "## Plugin Prompts\n\nInstalled Agent Portal plugins add this guidance:\n\n{}",
            parts.join("\n\n")
        )
    });
    (section, entries)
}

/// Say why each recorded plugin was loaded, from its resolution.
fn annotate_activation(
    entries: &mut [PluginContextEntry],
    resolutions: &crate::session_plugins::Resolutions,
) {
    for entry in entries {
        let activation = resolutions
            .get(&entry.plugin)
            .and_then(|resolution| resolution.activation)
            .unwrap_or(PluginActivation::Detected);
        entry.activation = activation;
        entry.reason = match activation {
            PluginActivation::Explicit => "Chosen for this session",
            PluginActivation::Policy => "Always loaded in this project",
            PluginActivation::Detected => "Detected in this directory",
        }
        .to_string();
    }
}

/// What a running session should switch to after its plugin selection
/// changed: the skills and prompts a fresh launch would now get, as text it
/// can act on without a restart. Skills are listed by path even for Claude,
/// whose native plugin directories were fixed at launch. Nothing is posted or
/// recorded here; the caller does both, in order, under its lock.
pub fn plugin_context_update(
    agent_type: AgentType,
    session_id: Uuid,
    working_directory: &Path,
) -> anyhow::Result<PluginContextChange> {
    let mut config = load_config();
    let resolutions =
        crate::session_plugins::apply_selection(&mut config, session_id, working_directory)?;
    let mut lines = Vec::new();
    let mut entries = Vec::new();
    for (plugin_name, plugin) in config.plugins.iter().filter(|(_, p)| p.enabled) {
        let root = PathBuf::from(&plugin.path);
        let Some(manifest) = read_plugin_manifest(&root) else {
            continue;
        };
        for skill in manifest
            .skills
            .iter()
            .filter(|skill| skill_applies_to(skill, agent_type.as_str()))
        {
            let path = root.join(&skill.path);
            if !path.is_file() {
                continue;
            }
            let metadata = read_skill_metadata(&path);
            let skill_name = metadata.name.unwrap_or_else(|| skill.name.clone());
            let description = metadata
                .description
                .map(|description| format!(" - {description}"))
                .unwrap_or_default();
            let line = format!(
                "- `{plugin_name}:{skill_name}`{description}: {}",
                path.display()
            );
            entries.push(available_skill(plugin_name, &skill_name, &path, &line));
            lines.push(line);
        }
    }
    let (prompts, prompt_entries) = plugin_prompts(&config);
    entries.extend(prompt_entries);
    annotate_activation(&mut entries, &resolutions);

    lines.sort();
    let skills = (!lines.is_empty()).then(|| {
        format!(
            "## Plugin Skills\n\nInstalled Agent Portal plugins provide these skills. When a user task matches one, read its `SKILL.md` completely before acting, then follow its instructions:\n\n{}",
            lines.join("\n")
        )
    });
    let current = [skills.clone(), prompts.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n");
    let update = format!(
        "## Plugin context changed\n\nThe user changed which Agent Portal plugins this session uses. This replaces every plugin skill and plugin prompt you were given earlier: use only what follows and disregard the rest.\n\n{}\n\n{}",
        skills.unwrap_or_else(|| "No plugin skills are enabled for this session now.".into()),
        prompts.unwrap_or_else(|| "No plugin prompts apply now.".into()),
    );
    Ok(PluginContextChange {
        current,
        update,
        entries,
    })
}

/// What a running session switches to when its plugin selection changes.
pub struct PluginContextChange {
    /// The plugin context a fresh conversation starts with from now on.
    pub current: String,
    /// The notice folded into the session's next input.
    pub update: String,
    /// The record the session card shows once the change is posted.
    pub entries: Vec<PluginContextEntry>,
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

pub(crate) fn write_config_atomic(path: &Path, contents: &str) -> anyhow::Result<()> {
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
    update_config_at(&config_path(), |config| {
        config.auth_token = Some(token.to_string());
        Ok(((), true))
    })?;
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
    update_config_at(path, |config| {
        config.backend_url = Some(backend_url.to_string());
        config.auth_token = Some(token.to_string());
        Ok(((), true))
    })
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
    let cleared = update_config_at(&config_path(), |config| {
        let cleared = !config.sessions.is_empty();
        config.sessions.clear();
        Ok((cleared, cleared))
    })?;
    if cleared {
        tracing::info!("Cleared launcher-local expected sessions from config");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[[prompts]]` reach every agent as text, and the record lists them.
    #[test]
    fn plugin_prompts_join_the_reminder_for_every_agent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("board");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("agent-portal-plugin.toml"),
            "name = \"board\"\n[[prompts]]\nname = \"conventions\"\npath = \"prompt.md\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("prompt.md"),
            "Nets are named in SCREAMING_CASE.\n",
        )
        .unwrap();
        let mut config = LauncherConfig::default();
        config.plugins.insert(
            "board".into(),
            InstalledPlugin {
                path: root.to_string_lossy().into_owned(),
                source: "s".into(),
                source_subdir: None,
                reference: None,
                enabled: true,
                installed_at: chrono::Utc::now(),
                revision: None,
            },
        );
        let generated = dir.path().join("generated");
        for agent in [AgentType::Claude, AgentType::Codex, AgentType::Muse] {
            let context = plugin_skill_context_from_config(&config, &generated, agent);
            let reminder = context.reminder.expect("prompts make a reminder");
            assert!(reminder.contains("## Plugin Prompts"));
            assert!(reminder.contains("### board: conventions"));
            assert!(reminder.contains("SCREAMING_CASE"));
            assert_eq!(context.injected.len(), 1);
            assert_eq!(context.injected[0].kind, PluginContextKind::Prompt);
        }

        config.plugins.get_mut("board").unwrap().enabled = false;
        let context = plugin_skill_context_from_config(&config, &generated, AgentType::Codex);
        assert!(context.reminder.is_none() && context.injected.is_empty());
    }

    /// A plugin change never saves over a config it couldn't read: that would
    /// silently drop every other install and the auth token.
    #[test]
    fn plugin_changes_leave_an_unreadable_config_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        let insert = |config: &mut LauncherConfig| {
            config.plugins.insert(
                "kicadmium".into(),
                InstalledPlugin {
                    path: "/p".into(),
                    source: "s".into(),
                    source_subdir: None,
                    reference: None,
                    enabled: true,
                    installed_at: chrono::Utc::now(),
                    revision: None,
                },
            );
            Ok(((), true))
        };

        std::fs::write(&path, "{ not json").unwrap();
        assert!(update_config_at(&path, insert).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");

        std::fs::remove_file(&path).unwrap();
        update_config_at(&path, insert).unwrap();
        assert!(load_config_at(&path).plugins.contains_key("kicadmium"));
        let removed = update_config_at(&path, |config| {
            let removed = config.plugins.remove("missing");
            let changed = removed.is_some();
            Ok((removed, changed))
        })
        .unwrap();
        assert!(removed.is_none());
    }

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
    fn credentials_never_save_over_a_config_that_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        let typo = r#"{"backend_url":"wss://mine","plugins":{"kicadmium":{}},}"#;
        std::fs::write(&path, typo).unwrap();

        let error = save_credentials_at(&path, "wss://default", "tok").unwrap_err();
        assert!(format!("{error:#}").contains("fix or move it aside"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), typo);
        // A missing file is a first run, not an error.
        assert_eq!(
            load_config_for_write_at(&dir.path().join("absent.json"))
                .unwrap()
                .auth_token,
            None
        );
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
            session_memory: None,
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
agents = ["claude", "codex", "antigravity"]
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
                revision: None,
            },
        );
        let mut config = LauncherConfig {
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
        // The record lists what was handed over, with its text for inspection.
        assert_eq!(context.injected.len(), 1);
        assert_eq!(context.injected[0].plugin, "kicad-pcb");
        assert_eq!(context.injected[0].name, "kicad-layout");
        assert!(context.injected[0].context_bytes > 0);
        assert!(!context.injected[0].text.is_empty());

        let context = plugin_skill_context_from_config(&config, &generated_root, AgentType::Codex);
        assert!(context.claude_plugin_dirs.is_empty());
        assert!(context.cleanup_dir.is_none());
        let reminder = context.reminder.unwrap();
        assert!(reminder.contains("`kicad-pcb:kicad-layout`"));
        assert!(reminder.contains("Route KiCad PCB layouts."));
        assert!(reminder.contains("nested/layout/SKILL.md"));

        let context = plugin_skill_context_from_config(&config, &generated_root, AgentType::Muse);
        assert!(context.reminder.is_none());
        assert!(
            context.injected.is_empty(),
            "nothing handed over, nothing recorded"
        );

        let context =
            plugin_skill_context_from_config(&config, &generated_root, AgentType::Antigravity);
        assert!(context.claude_plugin_dirs.is_empty());
        assert!(context.cleanup_dir.is_none());
        let reminder = context.reminder.unwrap();
        assert!(reminder.contains("`kicad-pcb:kicad-layout`"));

        let selection_path = dir.path().join("selection.json");
        std::fs::write(&selection_path, r#"{"kicad-pcb":false}"#).unwrap();
        crate::session_plugins::apply_selection_at(
            &mut config,
            &selection_path,
            &Default::default(),
            dir.path(),
        )
        .unwrap();
        for agent in [AgentType::Claude, AgentType::Codex, AgentType::Antigravity] {
            let context = plugin_skill_context_from_config(&config, &generated_root, agent);
            assert!(context.claude_plugin_dirs.is_empty());
            assert!(context.reminder.is_none());
            assert!(context.injected.is_empty());
        }
        assert!(!generated_root.join("kicad-pcb").exists());
    }

    #[test]
    fn roundtrip_config_with_session_id() {
        let sid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let config = LauncherConfig {
            session_memory: None,
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

#[cfg(test)]
mod session_memory_tests {
    use super::*;

    #[test]
    fn malformed_limits_never_fall_back_to_uncapped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        assert!(load_session_memory_at(&path).is_ok());
        for value in ["-1", "\"1024\"", "18446744073709551616"] {
            std::fs::write(
                &path,
                format!("{{\"session_memory\":{{\"max_bytes\":{value}}}}}"),
            )
            .unwrap();
            assert!(load_session_memory_at(&path).is_err(), "{value}");
        }
        std::fs::write(&path, r#"{"session_memory":{"max_bytes":1024}}"#).unwrap();
        assert_eq!(load_session_memory_at(&path).unwrap().max_bytes, Some(1024));
    }
}

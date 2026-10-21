//! Session-local skill selection. Host installs remain shared; overrides live
//! separately from launcher.json so editing one session cannot lose other config.
use crate::config::{self, LauncherConfig};
use crate::plugin_policy::DirectoryPolicy;
use anyhow::{Context, Result};
use shared::{
    api::{
        PluginActivation, PluginContextEntry, PluginInventoryResponse, PluginOverride,
        PluginPolicy, PortalPluginCommandInfo, PortalPluginInfo, PortalPluginSkillInfo,
    },
    plugin_manifest::InventoryManifest,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

type Selection = BTreeMap<String, bool>;
static SELECTION_WRITE: Mutex<()> = Mutex::new(());

fn selection_path(session: Uuid) -> PathBuf {
    session_lib::paths::config_dir()
        .join("session-plugins")
        .join(format!("{session}.json"))
}

fn read_selection(path: &Path) -> Result<Selection> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid session plugin selection"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Selection::new()),
        Err(e) => Err(e).context("Could not read session plugin selection"),
    }
}

fn read_manifest(plugin_root: &Path) -> Result<InventoryManifest> {
    fs::read_to_string(plugin_root.join("agent-portal-plugin.toml"))
        .context("Cannot read manifest")
        .and_then(|raw| toml::from_str::<InventoryManifest>(&raw).context("Invalid manifest"))
}

fn detect_rules(manifest: &InventoryManifest) -> Vec<crate::plugin_detect::DetectRule> {
    manifest
        .detect
        .iter()
        .map(|d| crate::plugin_detect::DetectRule {
            name: d.name.clone(),
            any: d.any.clone(),
        })
        .collect()
}

/// Whether a plugin's `[[detect]]` rules match `working_directory`. A plugin
/// without rules, or whose manifest can't be read, is never detected.
fn detected_in(plugin_root: &Path, working_directory: &Path) -> bool {
    read_manifest(plugin_root).is_ok_and(|manifest| {
        crate::plugin_detect::detect_reason(working_directory, &detect_rules(&manifest)).is_some()
    })
}

/// Why a plugin is, or isn't, loaded into a session.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Resolution {
    pub policy: PluginPolicy,
    pub activation: Option<PluginActivation>,
    /// Detected here, but the project's policy keeps it out.
    pub disabled_by_policy: bool,
}

pub(crate) type Resolutions = BTreeMap<String, Resolution>;

/// One plugin's effective state. An explicit per-session choice wins, then the
/// project's policy; with neither, a host-enabled plugin is on only where its
/// detect rules match.
fn resolve(
    host_enabled: bool,
    choice: Option<bool>,
    policy: PluginPolicy,
    detected: impl FnOnce() -> bool,
) -> (bool, Resolution) {
    let mut resolution = Resolution {
        policy,
        ..Default::default()
    };
    let enabled = match (choice, policy) {
        (Some(enabled), _) => {
            resolution.activation = Some(PluginActivation::Explicit);
            enabled
        }
        (None, PluginPolicy::Always) => {
            resolution.activation = Some(PluginActivation::Policy);
            true
        }
        (None, PluginPolicy::Never) => {
            resolution.activation = Some(PluginActivation::Policy);
            resolution.disabled_by_policy = host_enabled && detected();
            false
        }
        (None, PluginPolicy::Ask) => {
            let enabled = host_enabled && detected();
            resolution.activation = enabled.then_some(PluginActivation::Detected);
            enabled
        }
    };
    (enabled, resolution)
}

/// The session's effective plugins; see [`resolve`] for the precedence, so
/// every installed plugin doesn't load its skills into every session.
///
/// This selects which plugin *skills* a session is launched with, and is what
/// the inventory shows. It is not an authorization boundary: plugin commands
/// and runtimes (`plugin open`/`start`/commands) still go by the host-level
/// `enabled` flag. Detection runs at launch and on each inventory request, so
/// a file added afterwards changes the next launch, not the running session.
fn apply(
    config: &mut LauncherConfig,
    selection: &Selection,
    policy: &DirectoryPolicy,
    working_directory: &Path,
) -> Resolutions {
    let mut resolutions = Resolutions::new();
    for (name, plugin) in &mut config.plugins {
        let (enabled, resolution) = resolve(
            plugin.enabled,
            selection.get(name).copied(),
            policy.get(name).copied().unwrap_or_default(),
            || detected_in(Path::new(&plugin.path), working_directory),
        );
        plugin.enabled = enabled;
        resolutions.insert(name.clone(), resolution);
    }
    resolutions
}

pub(crate) fn apply_selection(
    config: &mut LauncherConfig,
    session: Uuid,
    working_directory: &Path,
) -> Result<Resolutions> {
    let policy = crate::plugin_policy::for_directory(working_directory)?;
    apply_selection_at(config, &selection_path(session), &policy, working_directory)
}

pub(crate) fn apply_selection_at(
    config: &mut LauncherConfig,
    path: &Path,
    policy: &DirectoryPolicy,
    working_directory: &Path,
) -> Result<Resolutions> {
    Ok(apply(
        config,
        &read_selection(path)?,
        policy,
        working_directory,
    ))
}

fn save_selection(path: &Path, config: &LauncherConfig, change: PluginOverride) -> Result<()> {
    anyhow::ensure!(
        config.plugins.contains_key(&change.name),
        "Plugin is not installed: {}",
        change.name
    );
    let _guard = SELECTION_WRITE
        .lock()
        .map_err(|_| anyhow::anyhow!("Plugin selection lock poisoned"))?;
    let mut selection = read_selection(path)?;
    selection.insert(change.name, change.enabled);
    config::write_config_atomic(path, &serde_json::to_string_pretty(&selection)?)
}

/// Persist requested child plugins before constructing its first agent process.
/// A later restart must preserve changes the owner made in the plugin panel.
pub(crate) fn install_initial_selection(
    session: Uuid,
    selection: &BTreeMap<String, bool>,
) -> Result<()> {
    let config = config::load_config();
    install_selection_at(&selection_path(session), &config, selection)
}
fn install_selection_at(path: &Path, config: &LauncherConfig, selection: &Selection) -> Result<()> {
    // A subsequent resume must keep owner edits even if the original plugin
    // was uninstalled since first launch.
    let _guard = SELECTION_WRITE
        .lock()
        .map_err(|_| anyhow::anyhow!("Plugin selection lock poisoned"))?;
    if path.exists() {
        return Ok(());
    }
    for name in selection.keys() {
        anyhow::ensure!(
            config.plugins.contains_key(name),
            "Plugin is not installed: {name}"
        );
    }
    config::write_config_atomic(path, &serde_json::to_string_pretty(selection)?)
}

pub(crate) fn request(
    session: Uuid,
    working_directory: String,
    selection: Option<PluginOverride>,
) -> Result<PluginInventoryResponse> {
    let mut config = config::load_config();
    if let Some(change) = selection {
        save_selection(&selection_path(session), &config, change)?;
    }
    let resolutions = apply_selection(&mut config, session, Path::new(&working_directory))?;
    let mut response = inventory(&config, Some(working_directory), true, &resolutions);
    response.injected_context = read_injected(&context_path(session));
    Ok(response)
}

/// What the session's current process was launched with, written at spawn.
fn context_path(session: Uuid) -> PathBuf {
    session_lib::paths::config_dir()
        .join("session-plugins")
        .join(format!("{session}.context.json"))
}

/// Record what a launch injected, replacing the previous launch's record.
pub(crate) fn record_injected(session: Uuid, entries: &[PluginContextEntry]) {
    let written = serde_json::to_string(entries)
        .map_err(anyhow::Error::from)
        .and_then(|json| config::write_config_atomic(&context_path(session), &json));
    if let Err(error) = written {
        tracing::warn!("Could not record plugin context for {session}: {error:#}");
    }
}

/// A session launched before recording existed, or whose record is damaged,
/// shows no record rather than a guess from the current inventory.
fn read_injected(path: &Path) -> Vec<PluginContextEntry> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Host inventory for the Plugins browser and launch suggestions. With a
/// directory, `enabled` is what a new session there would load; without one it
/// is the host-level install flag.
pub(crate) fn host_inventory(working_directory: Option<String>) -> Result<PluginInventoryResponse> {
    let mut config = config::load_config();
    let resolutions = match &working_directory {
        Some(directory) => {
            let directory = Path::new(directory);
            let policy = crate::plugin_policy::for_directory(directory)?;
            apply(&mut config, &Selection::new(), &policy, directory)
        }
        None => Resolutions::new(),
    };
    Ok(inventory(&config, working_directory, false, &resolutions))
}

/// The text a skill or prompt describes itself with: the frontmatter
/// `description:`, else its first non-empty line without heading marks.
fn file_description(contents: &str) -> Option<String> {
    let mut lines = contents.lines().map(str::trim);
    let first = lines.find(|line| !line.is_empty())?;
    let described = if first == "---" {
        lines
            .take_while(|line| *line != "---")
            .find_map(|line| line.strip_prefix("description:"))
            .map(|value| value.trim().trim_matches('"').to_string())
    } else {
        Some(first.trim_start_matches('#').trim().to_string())
    };
    described.filter(|text| !text.is_empty())
}

fn context_file(
    plugin_root: &Path,
    name: String,
    path: String,
    agents: Vec<String>,
    warnings: &mut Vec<String>,
) -> PortalPluginSkillInfo {
    let (description, bytes) = match fs::read_to_string(plugin_root.join(&path)) {
        Ok(contents) => (file_description(&contents), contents.len() as u64),
        Err(error) => {
            warnings.push(format!("Cannot read {path}: {error}"));
            (None, 0)
        }
    };
    PortalPluginSkillInfo {
        name,
        path,
        agents,
        description,
        context_bytes: bytes,
        estimated_tokens: bytes.div_ceil(4),
    }
}

fn inventory(
    config: &LauncherConfig,
    working_directory: Option<String>,
    session_selection_supported: bool,
    resolutions: &Resolutions,
) -> PluginInventoryResponse {
    let root = working_directory.as_deref().map(Path::new);
    let mut response = PluginInventoryResponse {
        working_directory: working_directory.clone(),
        scanned: root.is_some_and(Path::is_dir),
        session_selection_supported,
        ..Default::default()
    };
    for (name, installed) in &config.plugins {
        let plugin_root = Path::new(&installed.path);
        let manifest = read_manifest(plugin_root);
        let mut plugin = PortalPluginInfo {
            name: name.clone(),
            display_name: name.clone(),
            installed: true,
            enabled: installed.enabled,
            source: Some(installed.source.clone()),
            path: Some(installed.path.clone()),
            version: installed
                .revision
                .as_deref()
                .map(|revision| revision.chars().take(12).collect())
                .or_else(|| installed.reference.clone()),
            ..Default::default()
        };
        if let Some(resolution) = resolutions.get(name) {
            plugin.policy = resolution.policy;
            plugin.activation = resolution.activation;
            plugin.disabled_by_policy = resolution.disabled_by_policy;
        }
        match manifest {
            Err(error) => plugin.warnings.push(format!("{error:#}")),
            Ok(manifest) => {
                plugin.reason = root.and_then(|root| {
                    crate::plugin_detect::detect_reason(root, &detect_rules(&manifest))
                });
                plugin.display_name = manifest.display_name.unwrap_or_else(|| name.clone());
                plugin.description = manifest.description;
                plugin.suggested = plugin.reason.is_some();
                let mut warnings = Vec::new();
                for skill in manifest.skills {
                    plugin.skills.push(context_file(
                        plugin_root,
                        skill.name,
                        skill.path,
                        skill.agents,
                        &mut warnings,
                    ));
                }
                // The inventory view tolerates broken runtime fields so a bad
                // one never hides the plugin; read them from the strict view
                // and report a parse failure as a warning instead.
                match crate::plugin::load_manifest(plugin_root) {
                    Ok(runtime) => {
                        plugin.homepage = runtime.homepage;
                        plugin.has_doctor = runtime.install.doctor.is_some();
                        for prompt in runtime.prompts {
                            plugin.prompts.push(context_file(
                                plugin_root,
                                prompt.name,
                                prompt.path.to_string_lossy().into_owned(),
                                Vec::new(),
                                &mut warnings,
                            ));
                        }
                    }
                    Err(error) => warnings.push(format!("{error:#}")),
                }
                plugin.warnings.extend(warnings);
                // Agents the plugin's skills target; empty means every agent.
                let mut agents: Vec<String> = plugin
                    .skills
                    .iter()
                    .flat_map(|skill| skill.agents.iter().cloned())
                    .collect();
                agents.sort();
                agents.dedup();
                plugin.compatibility = agents;
                plugin.context_bytes = plugin.skills.iter().map(|s| s.context_bytes).sum();
                plugin.estimated_tokens = plugin.skills.iter().map(|s| s.estimated_tokens).sum();
                plugin.commands = manifest
                    .commands
                    .into_iter()
                    .map(|c| PortalPluginCommandInfo {
                        name: c.name,
                        description: c.description,
                        ..Default::default()
                    })
                    .collect();
                if let Some(surface) = manifest.surface {
                    plugin.surface_kind = surface.kind;
                    plugin.surface_title = surface.default_title;
                    plugin.surface_width_percent = surface.default_width_percent;
                }
            }
        }
        response.plugins.push(plugin);
    }
    response
        .plugins
        .sort_by(|a, b| a.display_name.cmp(&b.display_name));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_selection_is_validated_and_never_overwrites_owner_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selection.json");
        let config = LauncherConfig::default();
        let missing = [("not-installed".into(), true)].into();
        assert!(install_selection_at(&path, &config, &missing).is_err());
        assert!(!path.exists());
        install_selection_at(&path, &config, &Selection::new()).unwrap();
        std::fs::write(&path, r#"{"owner-change":false}"#).unwrap();
        install_selection_at(&path, &config, &missing).unwrap();
        assert_eq!(
            read_selection(&path).unwrap().get("owner-change"),
            Some(&false)
        );
    }

    fn config() -> LauncherConfig {
        let mut config = LauncherConfig::default();
        for name in ["kicadmium", "slowbooks"] {
            config.plugins.insert(
                name.into(),
                config::InstalledPlugin {
                    path: "/missing".into(),
                    source: "test".into(),
                    source_subdir: None,
                    reference: None,
                    enabled: true,
                    installed_at: chrono::Utc::now(),
                    revision: None,
                },
            );
        }
        config
    }
    #[test]
    fn choices_are_persistent_isolated_and_do_not_change_host_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-a.json");
        let original = config();
        save_selection(
            &path,
            &original,
            PluginOverride {
                name: "kicadmium".into(),
                enabled: false,
            },
        )
        .unwrap();
        save_selection(
            &path,
            &original,
            PluginOverride {
                name: "slowbooks".into(),
                enabled: false,
            },
        )
        .unwrap();
        let mut selected = config();
        apply(
            &mut selected,
            &read_selection(&path).unwrap(),
            &DirectoryPolicy::new(),
            Path::new("/missing"),
        );
        assert!(selected.plugins.values().all(|p| !p.enabled));
        assert!(original.plugins.values().all(|p| p.enabled));
        assert!(read_selection(&dir.path().join("session-b.json"))
            .unwrap()
            .is_empty());
        save_selection(
            &path,
            &original,
            PluginOverride {
                name: "slowbooks".into(),
                enabled: true,
            },
        )
        .unwrap();
        let selected = read_selection(&path).unwrap();
        assert!(!selected["kicadmium"]);
        assert!(selected["slowbooks"]);
    }
    #[test]
    fn unknown_plugins_and_corrupt_selections_are_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.json");
        assert!(save_selection(
            &path,
            &config(),
            PluginOverride {
                name: "unknown".into(),
                enabled: true
            }
        )
        .is_err());
        assert!(!path.exists());
        fs::write(&path, "broken").unwrap();
        assert!(save_selection(
            &path,
            &config(),
            PluginOverride {
                name: "slowbooks".into(),
                enabled: true
            }
        )
        .is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "broken");
    }
    #[test]
    fn inventory_uses_installed_config_including_disabled_or_broken_plugins() {
        let mut config = config();
        config.plugins.get_mut("slowbooks").unwrap().enabled = false;
        let report = inventory(&config, Some("/missing".into()), true, &Resolutions::new());
        assert!(report.session_selection_supported);
        assert_eq!(report.plugins.len(), 2);
        assert!(report.plugins[0].enabled);
        assert!(!report.plugins[1].enabled);
        assert!(!report.plugins[0].warnings.is_empty());
    }

    /// Without a per-session choice, only host-enabled plugins detected in the
    /// working directory are on; an explicit choice wins in both directions.
    #[test]
    fn unselected_plugins_default_to_those_detected_in_the_working_directory() {
        let plugins = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("board.kicad_pcb"), "").unwrap();
        let installs = [
            ("kicadmium", r#"any = ["*.kicad_pcb"]"#, true),
            ("unlinked", r#"any = ["*.slx"]"#, true),
            ("presentations", "", true),
            ("kicad-off", r#"any = ["*.kicad_pcb"]"#, false),
            ("broken", "", true),
        ];
        for (name, detect, _) in installs {
            let root = plugins.path().join(name);
            fs::create_dir_all(&root).unwrap();
            let rules = if detect.is_empty() {
                String::new()
            } else {
                format!("[[detect]]\nname = \"{name}\"\n{detect}\n")
            };
            let manifest = if name == "broken" {
                "this is = not [valid toml".to_string()
            } else {
                format!("name = \"{name}\"\n{rules}")
            };
            fs::write(root.join("agent-portal-plugin.toml"), manifest).unwrap();
        }
        let host = || {
            let mut config = LauncherConfig::default();
            for (name, _, enabled) in installs {
                config.plugins.insert(
                    name.into(),
                    config::InstalledPlugin {
                        path: plugins.path().join(name).to_string_lossy().into_owned(),
                        source: "test".into(),
                        source_subdir: None,
                        reference: None,
                        enabled,
                        installed_at: chrono::Utc::now(),
                        revision: None,
                    },
                );
            }
            config
        };
        fn enabled(config: &LauncherConfig) -> Vec<&str> {
            config
                .plugins
                .iter()
                .filter(|(_, p)| p.enabled)
                .map(|(n, _)| n.as_str())
                .collect()
        }

        // No choice: only the host-enabled, detected plugin. An unreadable
        // manifest is never detected, so `broken` stays off.
        let mut session = host();
        apply(
            &mut session,
            &Selection::new(),
            &DirectoryPolicy::new(),
            project.path(),
        );
        assert_eq!(enabled(&session), vec!["kicadmium"]);

        // Explicit choices win in both directions, including turning on a
        // plugin the host has disabled.
        let mut session = host();
        let choices = Selection::from([
            ("unlinked".into(), true),
            ("kicadmium".into(), false),
            ("kicad-off".into(), true),
        ]);
        apply(
            &mut session,
            &choices,
            &DirectoryPolicy::new(),
            project.path(),
        );
        assert_eq!(enabled(&session), vec!["kicad-off", "unlinked"]);

        // A session in a worktree under a hidden directory detects from that
        // root: only hidden *descendants* are skipped by the scan.
        let worktree = project.path().join(".worktrees").join("feature");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join("other.kicad_pcb"), "").unwrap();
        let mut session = host();
        apply(
            &mut session,
            &Selection::new(),
            &DirectoryPolicy::new(),
            &worktree,
        );
        assert_eq!(enabled(&session), vec!["kicadmium"]);
    }

    /// A session's own choice beats the project policy, which beats detection.
    #[test]
    fn choice_then_policy_then_detection() {
        use PluginActivation::*;
        use PluginPolicy::*;
        let on = |host, choice, policy, detected| resolve(host, choice, policy, || detected);

        assert!(!on(true, Some(false), Always, true).0);
        assert_eq!(
            on(false, Some(true), Never, false).1.activation,
            Some(Explicit)
        );

        let (enabled, r) = on(true, None, Always, false);
        assert!(enabled && r.activation == Some(Policy) && !r.disabled_by_policy);
        let (enabled, r) = on(true, None, Never, true);
        assert!(!enabled && r.activation == Some(Policy) && r.disabled_by_policy);
        let (_, r) = on(true, None, Never, false);
        assert!(
            !r.disabled_by_policy,
            "only a detected plugin is disabled *by* policy"
        );

        let (enabled, r) = on(true, None, Ask, true);
        assert!(enabled && r.activation == Some(Detected));
        let (enabled, r) = on(false, None, Ask, true);
        assert!(!enabled && r.activation.is_none());
    }

    /// A damaged or missing record reads as nothing recorded, never as a guess.
    #[test]
    fn injected_record_round_trips_and_damage_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.context.json");
        assert!(read_injected(&path).is_empty());
        let entry = PluginContextEntry {
            plugin: "kicadmium".into(),
            name: "layout".into(),
            path: "/p/skills/layout/SKILL.md".into(),
            kind: shared::api::PluginContextKind::Skill,
            activation: PluginActivation::Policy,
            reason: "Always loaded in this project".into(),
            context_bytes: 12,
            estimated_tokens: 3,
            text: "# Layout".into(),
        };
        fs::write(&path, serde_json::to_string(std::slice::from_ref(&entry)).unwrap()).unwrap();
        assert_eq!(read_injected(&path), vec![entry]);
        fs::write(&path, "broken").unwrap();
        assert!(read_injected(&path).is_empty());
    }

    #[test]
    fn file_description_reads_frontmatter_or_first_line() {
        let skill = "---\nname: layout\ndescription: \"Review a PCB layout\"\n---\n# Layout\n";
        assert_eq!(
            file_description(skill).as_deref(),
            Some("Review a PCB layout")
        );
        assert_eq!(
            file_description("\n# Export Gerbers\nbody").as_deref(),
            Some("Export Gerbers")
        );
        assert_eq!(file_description("---\nname: bare\n---\nbody"), None);
        assert_eq!(file_description(""), None);
    }

    /// The browser shows what a plugin contributes: prompts, docs, doctor,
    /// targeted agents, and where it is installed.
    #[test]
    fn inventory_describes_prompts_docs_and_agents() {
        let plugin = tempfile::tempdir().unwrap();
        fs::create_dir_all(plugin.path().join("skills")).unwrap();
        fs::write(
            plugin.path().join("skills/layout.md"),
            "---\ndescription: Layout review\n---\n",
        )
        .unwrap();
        fs::write(plugin.path().join("intro.md"), "# Board context\n").unwrap();
        fs::write(
            plugin.path().join("agent-portal-plugin.toml"),
            r#"
name = "kicadmium"
homepage = "https://example.com/kicadmium"
[install]
doctor = "true"
[[skills]]
name = "layout"
path = "skills/layout.md"
agents = ["codex", "claude"]
[[prompts]]
name = "intro"
path = "intro.md"
[[commands]]
name = "export"
run = "true"
"#,
        )
        .unwrap();
        let mut config = LauncherConfig::default();
        config.plugins.insert(
            "kicadmium".into(),
            config::InstalledPlugin {
                path: plugin.path().to_string_lossy().into_owned(),
                source: "github:meawoppl/agent-portal-plugins//kicadmium".into(),
                source_subdir: None,
                reference: None,
                enabled: true,
                installed_at: chrono::Utc::now(),
                revision: Some("0123456789abcdef".into()),
            },
        );

        let report = inventory(&config, None, false, &Resolutions::new());
        assert!(!report.scanned && !report.session_selection_supported);
        let info = &report.plugins[0];
        assert_eq!(
            info.homepage.as_deref(),
            Some("https://example.com/kicadmium")
        );
        assert!(info.has_doctor);
        assert_eq!(info.version.as_deref(), Some("0123456789ab"));
        assert_eq!(info.compatibility, vec!["claude", "codex"]);
        assert_eq!(info.skills[0].description.as_deref(), Some("Layout review"));
        assert_eq!(info.prompts[0].name, "intro");
        assert_eq!(
            info.prompts[0].description.as_deref(),
            Some("Board context")
        );
        assert!(info.reason.is_none() && info.warnings.is_empty());
    }
}

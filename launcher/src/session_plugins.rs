//! Session-local skill selection. Host installs remain shared; overrides live
//! separately from launcher.json so editing one session cannot lose other config.
use crate::config::{self, LauncherConfig};
use anyhow::{Context, Result};
use shared::{
    api::{
        PluginInventoryResponse, PluginOverride, PortalPluginCommandInfo, PortalPluginInfo,
        PortalPluginSkillInfo,
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

/// The session's effective plugins. An explicit per-session choice wins.
/// Without one, a host-enabled plugin is on only where its detect rules match
/// the session's working directory, so every installed plugin doesn't load its
/// skills into every session.
///
/// This selects which plugin *skills* a session is launched with, and is what
/// the inventory shows. It is not an authorization boundary: plugin commands
/// and runtimes (`plugin open`/`start`/commands) still go by the host-level
/// `enabled` flag. Detection runs at launch and on each inventory request, so
/// a file added afterwards changes the next launch, not the running session.
fn apply(config: &mut LauncherConfig, selection: &Selection, working_directory: &Path) {
    for (name, plugin) in &mut config.plugins {
        plugin.enabled = match selection.get(name) {
            Some(enabled) => *enabled,
            None => plugin.enabled && detected_in(Path::new(&plugin.path), working_directory),
        };
    }
}

pub(crate) fn apply_selection(
    config: &mut LauncherConfig,
    session: Uuid,
    working_directory: &Path,
) -> Result<()> {
    apply_selection_at(config, &selection_path(session), working_directory)
}

pub(crate) fn apply_selection_at(
    config: &mut LauncherConfig,
    path: &Path,
    working_directory: &Path,
) -> Result<()> {
    apply(config, &read_selection(path)?, working_directory);
    Ok(())
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
    apply_selection(&mut config, session, Path::new(&working_directory))?;
    Ok(inventory(&config, Some(working_directory), true))
}

/// Host inventory for the Plugins browser and launch suggestions. With a
/// directory, `enabled` is what a new session there would load; without one it
/// is the host-level install flag.
pub(crate) fn host_inventory(working_directory: Option<String>) -> PluginInventoryResponse {
    let mut config = config::load_config();
    if let Some(directory) = &working_directory {
        apply(&mut config, &Selection::new(), Path::new(directory));
    }
    inventory(&config, working_directory, false)
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
        let report = inventory(&config, Some("/missing".into()), true);
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
        apply(&mut session, &Selection::new(), project.path());
        assert_eq!(enabled(&session), vec!["kicadmium"]);

        // Explicit choices win in both directions, including turning on a
        // plugin the host has disabled.
        let mut session = host();
        let choices = Selection::from([
            ("unlinked".into(), true),
            ("kicadmium".into(), false),
            ("kicad-off".into(), true),
        ]);
        apply(&mut session, &choices, project.path());
        assert_eq!(enabled(&session), vec!["kicad-off", "unlinked"]);

        // A session in a worktree under a hidden directory detects from that
        // root: only hidden *descendants* are skipped by the scan.
        let worktree = project.path().join(".worktrees").join("feature");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join("other.kicad_pcb"), "").unwrap();
        let mut session = host();
        apply(&mut session, &Selection::new(), &worktree);
        assert_eq!(enabled(&session), vec!["kicadmium"]);
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

        let report = inventory(&config, None, false);
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

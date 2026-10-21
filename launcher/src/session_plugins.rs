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
    Ok(inventory(&config, working_directory))
}

fn inventory(config: &LauncherConfig, working_directory: String) -> PluginInventoryResponse {
    let root = Path::new(&working_directory);
    let mut response = PluginInventoryResponse {
        working_directory: Some(working_directory.clone()),
        scanned: root.is_dir(),
        session_selection_supported: true,
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
            ..Default::default()
        };
        match manifest {
            Err(error) => plugin.warnings.push(format!("{error:#}")),
            Ok(manifest) => {
                plugin.reason = crate::plugin_detect::detect_reason(root, &detect_rules(&manifest));
                plugin.display_name = manifest.display_name.unwrap_or_else(|| name.clone());
                plugin.description = manifest.description;
                plugin.suggested = plugin.reason.is_some();
                for skill in manifest.skills {
                    let bytes = fs::metadata(plugin_root.join(&skill.path))
                        .map(|m| m.len())
                        .unwrap_or(0);
                    plugin.skills.push(PortalPluginSkillInfo {
                        name: skill.name,
                        path: skill.path,
                        agents: skill.agents,
                        description: None,
                        context_bytes: bytes,
                        estimated_tokens: bytes.div_ceil(4),
                    });
                }
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
        let report = inventory(&config, "/missing".into());
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
        ];
        for (name, detect, _) in installs {
            let root = plugins.path().join(name);
            fs::create_dir_all(&root).unwrap();
            let rules = if detect.is_empty() {
                String::new()
            } else {
                format!("[[detect]]\nname = \"{name}\"\n{detect}\n")
            };
            fs::write(
                root.join("agent-portal-plugin.toml"),
                format!("name = \"{name}\"\n{rules}"),
            )
            .unwrap();
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

        let mut session = host();
        apply(&mut session, &Selection::new(), project.path());
        assert_eq!(enabled(&session), vec!["kicadmium"]);

        let mut session = host();
        let choices = Selection::from([("unlinked".into(), true), ("kicadmium".into(), false)]);
        apply(&mut session, &choices, project.path());
        assert_eq!(enabled(&session), vec!["unlinked"]);
    }
}

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

fn apply(config: &mut LauncherConfig, selection: &Selection) {
    for (name, plugin) in &mut config.plugins {
        if let Some(enabled) = selection.get(name) {
            plugin.enabled = *enabled;
        }
    }
}

pub(crate) fn apply_selection(config: &mut LauncherConfig, session: Uuid) -> Result<()> {
    apply_selection_at(config, &selection_path(session))
}

pub(crate) fn apply_selection_at(config: &mut LauncherConfig, path: &Path) -> Result<()> {
    apply(config, &read_selection(path)?);
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
    apply_selection(&mut config, session)?;
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
        let manifest = fs::read_to_string(plugin_root.join("agent-portal-plugin.toml"))
            .context("Cannot read manifest")
            .and_then(|raw| toml::from_str::<InventoryManifest>(&raw).context("Invalid manifest"));
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
                plugin.display_name = manifest.display_name.unwrap_or_else(|| name.clone());
                plugin.description = manifest.description;
                let detects = manifest
                    .detect
                    .into_iter()
                    .map(|d| crate::plugin_detect::DetectRule {
                        name: d.name,
                        any: d.any,
                    })
                    .collect::<Vec<_>>();
                plugin.reason = crate::plugin_detect::detect_reason(root, &detects);
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
        apply(&mut selected, &read_selection(&path).unwrap());
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
}

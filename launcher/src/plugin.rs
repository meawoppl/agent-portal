//! `agent-portal plugin` subcommands.
//!
//! This is intentionally launcher-local: plugins install into a user-owned
//! checkout directory, run on the same host as the agent session, and reuse the
//! existing `agent-portal forward` path for session surfaces.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{self, InstalledPlugin};

const MANIFEST: &str = "agent-portal-plugin.toml";
const SURFACE_STATE_FILE: &str = "surface.json";
const SURFACE_LOG_FILE: &str = "surface.log";
const SURFACE_LOG_TAIL_LINES: usize = 40;
const SURFACE_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct PluginManifest {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    install: InstallSection,
    #[serde(default)]
    surface: Option<SurfaceSection>,
    #[serde(default)]
    skills: Vec<SkillSection>,
    #[serde(default)]
    prompts: Vec<PromptSection>,
    #[serde(default)]
    commands: Vec<CommandSection>,
    #[serde(default)]
    toolchains: Vec<ToolchainSection>,
    #[serde(default)]
    capabilities: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct InstallSection {
    #[serde(default)]
    setup: Option<String>,
    #[serde(default)]
    doctor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SurfaceSection {
    #[serde(default)]
    default_title: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    stop: Option<String>,
    #[serde(default)]
    health_path: Option<String>,
    #[serde(default)]
    default_width_percent: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillSection {
    name: String,
    path: PathBuf,
    #[serde(default)]
    agents: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PromptSection {
    name: String,
    path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct CommandSection {
    name: String,
    #[serde(default)]
    description: Option<String>,
    run: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolchainSection {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    home: Option<PathBuf>,
    #[serde(default)]
    install: Option<String>,
    #[serde(default)]
    doctor: Option<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

struct PluginRuntime {
    name: String,
    root: PathBuf,
    installed: InstalledPlugin,
    manifest: PluginManifest,
    dirs: PluginRuntimeDirs,
}

struct PluginRuntimeDirs {
    portal: PathBuf,
    home: PathBuf,
    cache: PathBuf,
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    toolchains: PathBuf,
    surfaces: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SurfaceState {
    plugin: String,
    port: u16,
    pid: Option<u32>,
    /// `pid` leads its own process group, so stopping signals the whole
    /// group. Records written before surfaces got their own group lack this.
    #[serde(default)]
    process_group: bool,
    command: String,
    cwd: String,
    session_id: String,
    health_path: Option<String>,
    started_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
struct RuntimeJson {
    name: String,
    display_name: Option<String>,
    description: Option<String>,
    path: PathBuf,
    enabled: bool,
    plugin_home: PathBuf,
    toolchain_root: PathBuf,
    surface: Option<RuntimeSurfaceJson>,
    skills: Vec<RuntimeSkillJson>,
    commands: Vec<RuntimeCommandJson>,
    toolchains: Vec<RuntimeToolchainJson>,
    capabilities: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Debug, Serialize)]
struct RuntimeSurfaceJson {
    default_title: Option<String>,
    health_path: Option<String>,
    default_width_percent: Option<u8>,
    has_start: bool,
    has_stop: bool,
}

#[derive(Debug, Serialize)]
struct RuntimeSkillJson {
    name: String,
    path: PathBuf,
    agents: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RuntimeCommandJson {
    name: String,
    description: Option<String>,
    run: String,
}

#[derive(Debug, Serialize)]
struct RuntimeToolchainJson {
    name: String,
    description: Option<String>,
    home: PathBuf,
    has_install: bool,
    has_doctor: bool,
    env: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SurfaceHealth {
    Stopped,
    Healthy,
    Unhealthy,
    Exited,
}

impl SurfaceHealth {
    fn label(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Exited => "exited",
        }
    }
}

#[derive(Debug, Serialize)]
struct SurfaceStatusJson {
    plugin: String,
    state: SurfaceHealth,
    surface: Option<SurfaceState>,
    healthy: bool,
    log_tail: Vec<String>,
}

#[derive(Debug)]
struct SourceSpec {
    original: String,
    fetch: SourceFetch,
    subdir: PathBuf,
}

#[derive(Debug)]
enum SourceFetch {
    Git(String),
    Local(PathBuf),
}

pub fn list() -> Result<()> {
    let config = config::load_config();
    if config.plugins.is_empty() {
        println!("No plugins installed.");
        return Ok(());
    }
    for (name, installed) in &config.plugins {
        let state = if installed.enabled {
            "enabled"
        } else {
            "disabled"
        };
        println!("{name}\t{state}\t{}", installed.path);
    }
    Ok(())
}

pub fn info(name: &str) -> Result<()> {
    let runtime = load_runtime(name)?;
    let manifest = &runtime.manifest;
    println!(
        "{} ({name})",
        manifest.display_name.as_deref().unwrap_or(&runtime.name)
    );
    if let Some(description) = &manifest.description {
        println!("{description}");
    }
    if let Some(homepage) = &manifest.homepage {
        println!("homepage: {homepage}");
    }
    println!("path: {}", runtime.installed.path);
    println!("source: {}", runtime.installed.source);
    println!(
        "source_subdir: {}",
        runtime.installed.source_subdir.as_deref().unwrap_or(".")
    );
    println!(
        "status: {}",
        if runtime.installed.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!("plugin_home: {}", runtime.dirs.portal.display());
    if !manifest.skills.is_empty() {
        println!("skills:");
        for skill in &manifest.skills {
            println!(
                "  - {} [{}] {}",
                skill.name,
                if skill.agents.is_empty() {
                    "all".to_string()
                } else {
                    skill.agents.join(",")
                },
                skill.path.display()
            );
        }
    }
    if !manifest.prompts.is_empty() {
        println!("prompts:");
        for prompt in &manifest.prompts {
            println!("  - {} {}", prompt.name, prompt.path.display());
        }
    }
    if !manifest.commands.is_empty() {
        println!("commands:");
        for command in &manifest.commands {
            println!(
                "  - {} {}",
                command.name,
                command
                    .description
                    .as_deref()
                    .map(|value| format!("— {value}"))
                    .unwrap_or_default()
            );
        }
    }
    if !manifest.toolchains.is_empty() {
        println!("toolchains:");
        for toolchain in &manifest.toolchains {
            println!(
                "  - {} {}",
                toolchain.name,
                toolchain
                    .description
                    .as_deref()
                    .map(|value| format!("— {value}"))
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}

pub fn runtime(name: &str, json: bool) -> Result<()> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    if json {
        let value = RuntimeJson {
            name: runtime.name.clone(),
            display_name: runtime.manifest.display_name.clone(),
            description: runtime.manifest.description.clone(),
            path: runtime.root.clone(),
            enabled: runtime.installed.enabled,
            plugin_home: runtime.dirs.portal.clone(),
            toolchain_root: runtime.dirs.toolchains.clone(),
            surface: runtime
                .manifest
                .surface
                .as_ref()
                .map(|surface| RuntimeSurfaceJson {
                    default_title: surface.default_title.clone(),
                    health_path: surface.health_path.clone(),
                    default_width_percent: surface.default_width_percent,
                    has_start: surface.start.is_some(),
                    has_stop: surface.stop.is_some(),
                }),
            skills: runtime
                .manifest
                .skills
                .iter()
                .map(|skill| RuntimeSkillJson {
                    name: skill.name.clone(),
                    path: runtime.root.join(&skill.path),
                    agents: skill.agents.clone(),
                })
                .collect(),
            commands: runtime
                .manifest
                .commands
                .iter()
                .map(|command| RuntimeCommandJson {
                    name: command.name.clone(),
                    description: command.description.clone(),
                    run: command.run.clone(),
                })
                .collect(),
            toolchains: runtime
                .manifest
                .toolchains
                .iter()
                .map(|toolchain| RuntimeToolchainJson {
                    name: toolchain.name.clone(),
                    description: toolchain.description.clone(),
                    home: toolchain_home(&runtime, toolchain),
                    has_install: toolchain.install.is_some(),
                    has_doctor: toolchain.doctor.is_some(),
                    env: toolchain.env.clone(),
                })
                .collect(),
            capabilities: runtime.manifest.capabilities.clone(),
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        info(name)?;
        if let Some(surface) = &runtime.manifest.surface {
            println!("surface:");
            println!(
                "  title: {}",
                surface
                    .default_title
                    .as_deref()
                    .or(runtime.manifest.display_name.as_deref())
                    .unwrap_or(&runtime.name)
            );
            println!("  width: {}%", surface.default_width_percent.unwrap_or(50));
            println!(
                "  health: {}",
                surface.health_path.as_deref().unwrap_or("/")
            );
        }
    }
    Ok(())
}

pub fn skills(name: Option<&str>) -> Result<()> {
    let config = config::load_config();
    let mut rows = Vec::new();
    for (plugin_name, installed) in config.plugins {
        if let Some(name) = name {
            if plugin_name != name {
                continue;
            }
        }
        if !installed.enabled {
            continue;
        }
        let root = PathBuf::from(&installed.path);
        let manifest = load_manifest(&root)?;
        for skill in manifest.skills {
            let path = root.join(&skill.path);
            rows.push((
                plugin_name.clone(),
                skill.name,
                skill.agents,
                path.display().to_string(),
            ));
        }
    }
    if rows.is_empty() {
        println!("No plugin skills found.");
        return Ok(());
    }
    for (plugin, skill, agents, path) in rows {
        let agents = if agents.is_empty() {
            "all".to_string()
        } else {
            agents.join(",")
        };
        println!("{plugin}:{skill}\t{agents}\t{path}");
    }
    Ok(())
}

pub fn toolchains(name: &str, json: bool) -> Result<()> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    if json {
        let value = runtime
            .manifest
            .toolchains
            .iter()
            .map(|toolchain| RuntimeToolchainJson {
                name: toolchain.name.clone(),
                description: toolchain.description.clone(),
                home: toolchain_home(&runtime, toolchain),
                has_install: toolchain.install.is_some(),
                has_doctor: toolchain.doctor.is_some(),
                env: toolchain.env.clone(),
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    if runtime.manifest.toolchains.is_empty() {
        println!("plugin `{name}` declares no managed toolchains.");
        return Ok(());
    }
    for toolchain in &runtime.manifest.toolchains {
        println!(
            "{}\t{}\t{}",
            toolchain.name,
            toolchain
                .description
                .as_deref()
                .unwrap_or("managed plugin toolchain"),
            toolchain_home(&runtime, toolchain).display()
        );
    }
    Ok(())
}

pub fn setup(name: &str, toolchain_name: Option<&str>) -> Result<()> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    if let Some(toolchain_name) = toolchain_name {
        let toolchain = runtime
            .manifest
            .toolchains
            .iter()
            .find(|candidate| candidate.name == toolchain_name)
            .ok_or_else(|| {
                anyhow!("plugin `{name}` does not declare toolchain `{toolchain_name}`")
            })?;
        let Some(command) = toolchain.install.as_deref() else {
            bail!("toolchain `{toolchain_name}` does not declare an install command");
        };
        let home = toolchain_home(&runtime, toolchain);
        run_runtime_command(
            &runtime,
            command,
            Some(&[
                ("toolchain", toolchain.name.clone()),
                ("toolchain_home", home.display().to_string()),
            ]),
        )
    } else if let Some(command) = runtime.manifest.install.setup.as_deref() {
        run_runtime_command(&runtime, command, None)
    } else {
        bail!("plugin `{name}` does not declare install.setup")
    }
}

pub fn install(
    source: &str,
    name: Option<&str>,
    reference: Option<&str>,
    force: bool,
) -> Result<()> {
    let spec = parse_source(source)?;
    let staging = materialize_source(&spec, reference)?;
    let source_root = staging.path().join(&spec.subdir);
    let manifest = load_manifest(&source_root)?;
    let install_name = name.unwrap_or(&manifest.name);
    validate_plugin_name(install_name)?;
    if install_name != manifest.name {
        bail!(
            "install name `{install_name}` does not match manifest name `{}`",
            manifest.name
        );
    }
    let revision = git_revision(staging.path());

    let registered = config::load_config().plugins.get(install_name).cloned();
    let dest = registered
        .as_ref()
        .map(|installed| PathBuf::from(&installed.path))
        .unwrap_or_else(|| config::plugin_root().join(install_name));
    recover_interrupted_replace(install_name, &dest)?;
    if registered.is_some() {
        if !force {
            bail!(
                "plugin `{install_name}` is already installed; use `agent-portal plugin update {install_name}` or `install --force`"
            );
        }
        if let Ok(runtime) = load_runtime(install_name) {
            halt_surface_quietly(&runtime);
        }
    } else if dest.exists() {
        // An unregistered directory under the plugin root is the debris of an
        // install that failed before registering. Clear it so the retry can
        // proceed, but only when it is recognisably that plugin (or forced).
        let leftover = load_manifest(&dest)
            .map(|m| m.name == install_name)
            .unwrap_or(false);
        if !leftover && !force {
            bail!(
                "{} exists but is not an install of `{install_name}`; move it aside or pass --force",
                dest.display()
            );
        }
        println!(
            "Removing unregistered leftover directory {}",
            dest.display()
        );
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("failed to remove {}", dest.display()))?;
    }
    let record = InstalledPlugin {
        path: dest.display().to_string(),
        source: spec.original,
        source_subdir: Some(path_display(&spec.subdir)),
        reference: reference.map(str::to_string),
        enabled: true,
        installed_at: chrono::Utc::now(),
        revision,
    };
    replace_checkout(
        install_name,
        &source_root,
        &dest,
        manifest.install.setup.as_deref(),
        false,
        || config::save_installed_plugin(install_name, record),
    )?;
    println!("Installed {install_name} to {}", dest.display());
    Ok(())
}

/// Re-fetch an installed plugin from its recorded source and re-run setup.
/// The plugin-local `.portal` state (toolchains, caches, data) is carried
/// across, and the previous checkout is restored if anything fails.
pub fn update(name: &str, check: bool) -> Result<()> {
    let installed = installed_plugin(name)?;
    if check {
        match update_available(&installed) {
            Some(true) => println!("{name}: update available"),
            Some(false) => println!("{name}: up to date"),
            None => println!("{name}: unknown (source is not a git repository)"),
        }
        return Ok(());
    }
    let spec = parse_source(&installed.source)?;
    let staging = materialize_source(&spec, installed.reference.as_deref())?;
    let source_root = staging.path().join(&spec.subdir);
    let manifest = load_manifest(&source_root)?;
    if manifest.name != name {
        bail!(
            "source {} now provides plugin `{}`, not `{name}`",
            installed.source,
            manifest.name
        );
    }
    let revision = git_revision(staging.path());

    let dest = PathBuf::from(&installed.path);
    recover_interrupted_replace(name, &dest)?;
    if let Ok(runtime) = load_runtime(name) {
        halt_surface_quietly(&runtime);
    }
    let unchanged = revision.is_some() && revision == installed.revision;
    let record = InstalledPlugin {
        installed_at: chrono::Utc::now(),
        revision,
        ..installed
    };
    replace_checkout(
        name,
        &source_root,
        &dest,
        manifest.install.setup.as_deref(),
        true,
        || config::save_installed_plugin(name, record),
    )?;
    if unchanged {
        println!("{name} was already up to date; setup re-ran.");
    } else {
        println!("Updated {name}.");
    }
    Ok(())
}

fn replace_backup_path(name: &str, dest: &Path) -> PathBuf {
    dest.with_file_name(format!(".{name}.replace-backup"))
}

/// A backup only outlives a replacement that was interrupted mid-flight (the
/// launcher was killed). It is the last known-good checkout, so restore it
/// before anything else touches the directory; never delete it blindly.
fn recover_interrupted_replace(name: &str, dest: &Path) -> Result<()> {
    let backup = replace_backup_path(name, dest);
    if !backup.exists() {
        return Ok(());
    }
    // The interrupted run may already have moved the plugin-local state into
    // the partial checkout; put it back before discarding that checkout.
    let partial_state = dest.join(".portal");
    let backup_state = backup.join(".portal");
    if partial_state.exists() && !backup_state.exists() {
        std::fs::rename(&partial_state, &backup_state).with_context(|| {
            format!(
                "failed to move {} back into {}",
                partial_state.display(),
                backup.display()
            )
        })?;
    }
    if dest.exists() {
        std::fs::remove_dir_all(dest)
            .with_context(|| format!("failed to clear partial {}", dest.display()))?;
    }
    std::fs::rename(&backup, dest).with_context(|| {
        format!(
            "failed to restore {} from {}",
            dest.display(),
            backup.display()
        )
    })?;
    eprintln!(
        "Restored {} from an interrupted install/update",
        dest.display()
    );
    Ok(())
}

/// Put the plugin at `source_root` in place at `dest`, run its setup, and
/// `register` it, as one transaction: an existing checkout is moved aside
/// first and restored if the copy, setup, or registration fails, and a failed
/// fresh install leaves nothing behind. Either way no unregistered,
/// half-built directory survives to block a retry. `carry_state` moves the
/// old checkout's `.portal` (managed toolchains, caches, data) into the new
/// one, and back again on rollback.
fn replace_checkout(
    name: &str,
    source_root: &Path,
    dest: &Path,
    setup: Option<&str>,
    carry_state: bool,
    register: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let backup = replace_backup_path(name, dest);
    let had_previous = dest.exists();
    if had_previous {
        std::fs::rename(dest, &backup)
            .with_context(|| format!("failed to move {} aside", dest.display()))?;
    }
    let old_state = backup.join(".portal");
    let new_state = dest.join(".portal");
    let result = (|| -> Result<()> {
        copy_dir(source_root, dest).with_context(|| {
            format!(
                "failed to copy plugin from {} to {}",
                source_root.display(),
                dest.display()
            )
        })?;
        if carry_state && old_state.exists() {
            if new_state.exists() {
                std::fs::remove_dir_all(&new_state)?;
            }
            std::fs::rename(&old_state, &new_state)?;
        }
        if let Some(setup) = setup {
            println!("Running setup: {setup}");
            run_manifest_command(dest, setup)?;
        }
        register()
    })();
    let Err(err) = result else {
        if had_previous {
            let _ = std::fs::remove_dir_all(&backup);
        }
        return Ok(());
    };
    if carry_state && had_previous && new_state.exists() && !old_state.exists() {
        std::fs::rename(&new_state, &old_state).with_context(|| {
            format!(
                "{err:#}; moving {} back into {} also failed",
                new_state.display(),
                backup.display()
            )
        })?;
    }
    if dest.exists() {
        std::fs::remove_dir_all(dest).with_context(|| {
            format!(
                "{err:#}; removing the partial {} also failed",
                dest.display()
            )
        })?;
    }
    if had_previous {
        std::fs::rename(&backup, dest).with_context(|| {
            format!(
                "{err:#}; restoring {} from {} also failed",
                dest.display(),
                backup.display()
            )
        })?;
        return Err(err.context(format!(
            "`{name}` failed to install; the previous version was restored"
        )));
    }
    Err(err.context(format!(
        "`{name}` failed to install; the partial install was removed, so it is safe to retry"
    )))
}

pub fn remove(name: &str) -> Result<()> {
    validate_plugin_name(name)?;
    if !remove_installed(name)? {
        bail!("plugin `{name}` is not installed");
    }
    println!("Removed {name}.");
    Ok(())
}

/// Stop the plugin's surface, unregister it, and delete its directory. Also
/// clears an unregistered directory left by a failed install. Returns whether
/// there was anything to remove.
fn remove_installed(name: &str) -> Result<bool> {
    if let Ok(runtime) = load_runtime(name) {
        halt_surface_quietly(&runtime);
    }
    let removed = config::remove_installed_plugin(name)?;
    let path = match &removed {
        Some(installed) => PathBuf::from(&installed.path),
        None => config::plugin_root().join(name),
    };
    let existed = path.exists();
    if existed {
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(removed.is_some() || existed)
}

pub fn set_enabled(name: &str, enabled: bool) -> Result<()> {
    config::set_plugin_enabled(name, enabled)?;
    println!("{name} {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

pub fn doctor(name: &str) -> Result<()> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    let Some(command) = runtime.manifest.install.doctor.as_deref() else {
        bail!("plugin `{name}` does not declare an install.doctor command");
    };
    run_runtime_command(&runtime, command, None)
}

pub async fn open(name: &str) -> Result<()> {
    let state = start(name).await?;
    eprintln!("Opened {} on 127.0.0.1:{}", state.plugin, state.port);
    crate::forward::open(state.port).await
}

pub async fn start(name: &str) -> Result<SurfaceState> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let (state, reused) = start_surface(&runtime, &cwd, &current_session_id()).await?;
    if reused {
        println!("plugin `{name}` surface already running on {}", state.port);
    } else {
        println!("started `{name}` surface on 127.0.0.1:{}", state.port);
    }
    Ok(state)
}

pub async fn restart(name: &str) -> Result<SurfaceState> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    if let Some(state) = surface_state(&runtime)? {
        halt_surface(&runtime, &state)?;
    }
    start(name).await
}

/// Start the plugin's surface for `cwd`/`session_id`, or reuse a healthy one.
/// Returns the surface record and whether it was reused.
async fn start_surface(
    runtime: &PluginRuntime,
    cwd: &Path,
    session_id: &str,
) -> Result<(SurfaceState, bool)> {
    let name = &runtime.name;
    if cfg!(not(unix)) {
        bail!("plugin surfaces are only supported on Linux and macOS");
    }
    if let Some(state) = surface_state(runtime)? {
        if surface_healthy(&state).await {
            return Ok((state, true));
        }
        // A stale record may still have a live but unhealthy process tree
        // behind it; reap it before starting a replacement.
        halt_surface(runtime, &state)?;
    }
    let surface = runtime
        .manifest
        .surface
        .as_ref()
        .ok_or_else(|| anyhow!("plugin `{name}` does not declare a surface"))?;
    let start = surface
        .start
        .as_deref()
        .ok_or_else(|| anyhow!("plugin `{name}` surface does not declare a start command"))?;
    let port = free_port()?;
    let command = expand_runtime_command(
        runtime,
        start,
        Some(&[
            ("port", port.to_string()),
            ("session_id", session_id.to_string()),
            ("cwd", cwd.display().to_string()),
        ]),
    );
    let log_path = surface_log_path(runtime);
    let mut child = spawn_surface(runtime, &command, &log_path)?;
    let pid = child.id();
    // Reap the child as soon as it exits, whatever happens below: a zombie
    // leader keeps its process group looking alive, and a long-lived launcher
    // must not accumulate them.
    let exited = std::sync::Arc::new(std::sync::OnceLock::new());
    {
        let exited = exited.clone();
        std::thread::spawn(move || {
            if let Ok(status) = child.wait() {
                let _ = exited.set(status);
            }
        });
    }
    let state = SurfaceState {
        plugin: runtime.name.clone(),
        port,
        pid: Some(pid),
        process_group: cfg!(unix),
        command,
        cwd: cwd.display().to_string(),
        session_id: session_id.to_string(),
        health_path: surface.health_path.clone(),
        started_at: chrono::Utc::now(),
    };
    // Record the surface before waiting on health so a stop issued during
    // startup still finds (and reaps) it. Any failure from here on tears the
    // spawned tree down rather than orphaning it.
    let started = match write_surface_state(runtime, &state) {
        Ok(()) => wait_for_health(&exited, port, surface.health_path.as_deref()).await,
        Err(err) => Err(err),
    };
    if let Err(err) = started {
        let _ = stop_surface_process(&state);
        let _ = std::fs::remove_file(surface_state_path(runtime));
        let tail = log_tail(&log_path, SURFACE_LOG_TAIL_LINES).join("\n");
        if tail.is_empty() {
            return Err(err);
        }
        return Err(err.context(format!("surface log tail:\n{tail}")));
    }
    Ok((state, false))
}

pub async fn status(name: &str, json: bool) -> Result<()> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    let status = surface_status(&runtime).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else if let Some(state) = &status.surface {
        println!(
            "{}\t{}\t127.0.0.1:{}\tpid={}",
            name,
            status.state.label(),
            state.port,
            state
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
        if status.state != SurfaceHealth::Healthy && !status.log_tail.is_empty() {
            println!("--- {} ---", surface_log_path(&runtime).display());
            for line in &status.log_tail {
                println!("{line}");
            }
        }
    } else {
        println!("{name}\tstopped");
    }
    Ok(())
}

async fn surface_status(runtime: &PluginRuntime) -> Result<SurfaceStatusJson> {
    let surface = surface_state(runtime)?;
    let state = match &surface {
        None => SurfaceHealth::Stopped,
        Some(record) if !surface_alive(record) => SurfaceHealth::Exited,
        Some(record) if surface_healthy(record).await => SurfaceHealth::Healthy,
        Some(_) => SurfaceHealth::Unhealthy,
    };
    let log_tail = if surface.is_some() || surface_log_path(runtime).exists() {
        log_tail(&surface_log_path(runtime), SURFACE_LOG_TAIL_LINES)
    } else {
        Vec::new()
    };
    Ok(SurfaceStatusJson {
        plugin: runtime.name.clone(),
        healthy: state == SurfaceHealth::Healthy,
        state,
        surface,
        log_tail,
    })
}

pub fn stop(name: &str) -> Result<()> {
    let runtime = load_runtime(name)?;
    let Some(state) = surface_state(&runtime)? else {
        println!("plugin `{name}` surface is not running.");
        return Ok(());
    };
    halt_surface(&runtime, &state)?;
    println!("stopped `{name}` surface.");
    Ok(())
}

/// Stop a recorded surface: run the manifest `stop` command if declared, then
/// make sure the whole process tree is gone before dropping the record.
fn halt_surface(runtime: &PluginRuntime, state: &SurfaceState) -> Result<()> {
    if let Some(stop) = runtime
        .manifest
        .surface
        .as_ref()
        .and_then(|surface| surface.stop.as_deref())
    {
        if let Err(err) = run_runtime_command(
            runtime,
            stop,
            Some(&[
                ("port", state.port.to_string()),
                ("session_id", state.session_id.clone()),
                ("cwd", state.cwd.clone()),
            ]),
        ) {
            eprintln!("warning: surface stop command failed: {err:#}");
        }
    }
    stop_surface_process(state)?;
    let _ = std::fs::remove_file(surface_state_path(runtime));
    Ok(())
}

/// Best-effort surface stop ahead of a remove/reinstall, so the binary is not
/// deleted out from under a live server.
fn halt_surface_quietly(runtime: &PluginRuntime) {
    if let Ok(Some(state)) = surface_state(runtime) {
        if let Err(err) = halt_surface(runtime, &state) {
            eprintln!(
                "warning: failed to stop `{}` surface: {err:#}",
                runtime.name
            );
        }
    }
}

fn installed_plugin(name: &str) -> Result<InstalledPlugin> {
    config::load_config()
        .plugins
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow!("plugin `{name}` is not installed"))
}

fn load_runtime(name: &str) -> Result<PluginRuntime> {
    let installed = installed_plugin(name)?;
    let root = PathBuf::from(&installed.path);
    let manifest = load_manifest(&root)?;
    let dirs = PluginRuntimeDirs::new(&root);
    Ok(PluginRuntime {
        name: name.to_string(),
        root,
        installed,
        manifest,
        dirs,
    })
}

impl PluginRuntimeDirs {
    fn new(root: &Path) -> Self {
        let portal = root.join(".portal");
        Self {
            home: portal.join("home"),
            cache: portal.join("cache"),
            config: portal.join("config"),
            data: portal.join("data"),
            state: portal.join("state"),
            toolchains: portal.join("toolchains"),
            surfaces: portal.join("state").join("surfaces"),
            portal,
        }
    }
}

fn ensure_enabled(name: &str, plugin: &InstalledPlugin) -> Result<()> {
    if plugin.enabled {
        Ok(())
    } else {
        bail!("plugin `{name}` is disabled")
    }
}

fn load_manifest(root: &Path) -> Result<PluginManifest> {
    let path = root.join(MANIFEST);
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&body).with_context(|| format!("failed to parse {}", path.display()))
}

fn validate_plugin_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        bail!("invalid plugin name `{name}`; use lowercase letters, digits, and hyphens")
    }
}

fn parse_source(source: &str) -> Result<SourceSpec> {
    let (base, subdir) = split_subdir(source);
    let subdir = subdir
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let fetch = if let Some(repo) = base.strip_prefix("github:") {
        SourceFetch::Git(format!("https://github.com/{repo}.git"))
    } else if base.starts_with("https://") || base.starts_with("git@") {
        SourceFetch::Git(base.to_string())
    } else {
        SourceFetch::Local(PathBuf::from(base))
    };
    Ok(SourceSpec {
        original: source.to_string(),
        fetch,
        subdir,
    })
}

fn split_subdir(source: &str) -> (&str, Option<&str>) {
    if let Some(rest) = source.strip_prefix("github:") {
        if let Some((repo, subdir)) = rest.split_once("//") {
            let base_len = "github:".len() + repo.len();
            return (&source[..base_len], Some(subdir.trim_start_matches('/')));
        }
        return (source, None);
    }
    if let Some((base, subdir)) = source.split_once(".git//") {
        let base_len = base.len() + ".git".len();
        return (&source[..base_len], Some(subdir.trim_start_matches('/')));
    }
    if let Some((base, subdir)) = source.rsplit_once("//") {
        if base.starts_with('.') || base.starts_with('/') {
            return (base, Some(subdir.trim_start_matches('/')));
        }
    }
    (source, None)
}

/// A fetched plugin source in a temp directory, deleted on drop.
struct StagingDir(PathBuf);

impl StagingDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn materialize_source(spec: &SourceSpec, reference: Option<&str>) -> Result<StagingDir> {
    let dir = StagingDir(std::env::temp_dir().join(format!(
        "agent-portal-plugin-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )));
    match &spec.fetch {
        SourceFetch::Git(url) => {
            run_cmd(
                Path::new("."),
                "git",
                &[
                    OsStr::new("clone"),
                    OsStr::new("--depth"),
                    OsStr::new("1"),
                    OsStr::new(url),
                    dir.path().as_os_str(),
                ],
            )?;
            if let Some(reference) = reference {
                run_cmd(
                    dir.path(),
                    "git",
                    &[
                        OsStr::new("fetch"),
                        OsStr::new("origin"),
                        OsStr::new(reference),
                    ],
                )?;
                run_cmd(
                    dir.path(),
                    "git",
                    &[OsStr::new("checkout"), OsStr::new(reference)],
                )?;
            }
        }
        SourceFetch::Local(path) => {
            let path = path
                .canonicalize()
                .with_context(|| format!("failed to resolve {}", path.display()))?;
            copy_dir(&path, dir.path())?;
        }
    }
    Ok(dir)
}

/// Full commit of a git checkout, if `dir` is one.
fn git_revision(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|rev| !rev.is_empty())
}

/// Whether the recorded git source has moved past the installed revision.
/// `None` when the source is not git or the remote cannot be reached.
pub(crate) fn update_available(installed: &InstalledPlugin) -> Option<bool> {
    let installed_rev = installed.revision.as_deref()?;
    let SourceFetch::Git(url) = parse_source(&installed.source).ok()?.fetch else {
        return None;
    };
    let reference = installed.reference.as_deref().unwrap_or("HEAD");
    let output = Command::new("git")
        .args(["ls-remote", &url, reference])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let remote = stdout.split_whitespace().next()?;
    Some(remote != installed_rev)
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    if !src.is_dir() {
        bail!("{} is not a directory", src.display());
    }
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_dir(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to)?;
            let perms = std::fs::metadata(&from)?.permissions();
            std::fs::set_permissions(&to, perms)?;
        }
    }
    Ok(())
}

fn run_manifest_command(root: &Path, command: &str) -> Result<()> {
    let runtime = PluginRuntime {
        name: root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("plugin")
            .to_string(),
        root: root.to_path_buf(),
        installed: InstalledPlugin {
            path: root.display().to_string(),
            source: "manifest-command".to_string(),
            source_subdir: None,
            reference: None,
            enabled: true,
            installed_at: chrono::Utc::now(),
            revision: None,
        },
        manifest: load_manifest(root)?,
        dirs: PluginRuntimeDirs::new(root),
    };
    run_runtime_command(&runtime, command, None)
}

fn run_runtime_command(
    runtime: &PluginRuntime,
    command: &str,
    extra: Option<&[(&str, String)]>,
) -> Result<()> {
    prepare_plugin_home(&runtime.root)?;
    let command = expand_runtime_command(runtime, command, extra);
    let status = shell_command(runtime, &command)
        .status()
        .with_context(|| format!("failed to run `{command}`"))?;
    if status.success() {
        Ok(())
    } else if status.code() == Some(127) {
        bail!(
            "command `{command}` exited with {status}: a program it runs was not found on PATH ({}). \
             Install it, or add its directory to the launcher service's PATH",
            plugin_path_env()
        )
    } else {
        bail!("command `{command}` exited with {status}")
    }
}

/// Spawn a surface server with output captured to `log_path`. On unix it
/// leads its own process group so a stop reaps the whole tree, not just the
/// `sh -c` wrapper.
fn spawn_surface(
    runtime: &PluginRuntime,
    command: &str,
    log_path: &Path,
) -> Result<std::process::Child> {
    prepare_plugin_home(&runtime.root)?;
    let log = std::fs::File::create(log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;
    let mut cmd = shell_command(runtime, command);
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
        .with_context(|| format!("failed to start `{command}`"))
}

/// Whether any process from a recorded surface is still alive. A surface
/// spawned as a group leader is probed as a group, so children that outlive
/// the leader still count.
#[cfg(unix)]
fn surface_alive(state: &SurfaceState) -> bool {
    let Some(pid) = state.pid else {
        return false;
    };
    let target = if state.process_group {
        -(pid as i32)
    } else {
        pid as i32
    };
    // SAFETY: signal 0 only probes for existence; nothing is delivered.
    unsafe { libc::kill(target, 0) == 0 }
}

#[cfg(not(unix))]
fn surface_alive(state: &SurfaceState) -> bool {
    state.pid.is_some()
}

/// Terminate a surface's process tree: SIGTERM, wait, then SIGKILL. Errors
/// if anything survives, so callers never report a surface stopped while it
/// still holds its port.
#[cfg(unix)]
fn stop_surface_process(state: &SurfaceState) -> Result<()> {
    let Some(pid) = state.pid else {
        return Ok(());
    };
    let signal = |sig: libc::c_int| {
        if state.process_group {
            session_lib::session::signal_process_group(pid, sig);
        } else {
            // A record from before surfaces had their own group: the pid is
            // the `sh -c` wrapper, so signal its children explicitly too.
            let _ = Command::new("pkill")
                .arg(format!("-{sig}"))
                .args(["-P", &pid.to_string()])
                .status();
            // SAFETY: plain kill(2) of a single recorded pid.
            unsafe {
                libc::kill(pid as i32, sig);
            }
        }
    };
    for (sig, grace) in [
        (libc::SIGTERM, Duration::from_secs(5)),
        (libc::SIGKILL, Duration::from_secs(2)),
    ] {
        if !surface_alive(state) {
            return Ok(());
        }
        signal(sig);
        let deadline = std::time::Instant::now() + grace;
        while surface_alive(state) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if surface_alive(state) {
        bail!("surface process {pid} is still running after SIGKILL");
    }
    Ok(())
}

#[cfg(not(unix))]
fn stop_surface_process(state: &SurfaceState) -> Result<()> {
    if state.pid.is_some() {
        eprintln!(
            "plugin `{}` declares no stop command; removing surface state only",
            state.plugin
        );
    }
    Ok(())
}

fn surface_log_path(runtime: &PluginRuntime) -> PathBuf {
    runtime.dirs.surfaces.join(SURFACE_LOG_FILE)
}

/// Last `lines` lines of a log file (empty if missing).
fn log_tail(path: &Path, lines: usize) -> Vec<String> {
    let Ok(body) = std::fs::read(path) else {
        return Vec::new();
    };
    let body = String::from_utf8_lossy(&body);
    let all: Vec<&str> = body.lines().collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|line| line.to_string())
        .collect()
}

fn prepare_plugin_home(root: &Path) -> Result<()> {
    let dirs = PluginRuntimeDirs::new(root);
    for dir in [
        &dirs.home,
        &dirs.cache,
        &dirs.config,
        &dirs.data,
        &dirs.state,
        &dirs.toolchains,
        &dirs.surfaces,
    ] {
        std::fs::create_dir_all(dir).with_context(|| {
            format!(
                "failed to prepare plugin support directory {}",
                dir.display()
            )
        })?;
    }
    Ok(())
}

fn shell_command(runtime: &PluginRuntime, command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(&runtime.root)
        .envs(plugin_command_env(runtime));
    cmd
}

fn plugin_command_env(runtime: &PluginRuntime) -> Vec<(&'static str, String)> {
    let mut env = vec![
        (
            "AGENT_PORTAL_PLUGIN_DIR",
            runtime.root.display().to_string(),
        ),
        (
            "AGENT_PORTAL_PLUGIN_HOME",
            runtime.dirs.portal.display().to_string(),
        ),
        (
            "AGENT_PORTAL_PLUGIN_TOOLCHAIN_ROOT",
            runtime.dirs.toolchains.display().to_string(),
        ),
        ("HOME", runtime.dirs.home.display().to_string()),
        ("XDG_CACHE_HOME", runtime.dirs.cache.display().to_string()),
        ("XDG_CONFIG_HOME", runtime.dirs.config.display().to_string()),
        ("XDG_DATA_HOME", runtime.dirs.data.display().to_string()),
        ("XDG_STATE_HOME", runtime.dirs.state.display().to_string()),
        ("PATH", plugin_path_env()),
    ];
    // HOME is redirected to the plugin's private home, which would hide the
    // user's rustup/cargo installs from the `cargo` proxy. Point it back at
    // them explicitly so source-built plugins can set up.
    if let Some(home) = dirs::home_dir() {
        for (key, default) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
            let dir = std::env::var_os(key)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(default));
            if dir.is_dir() {
                env.push((key, dir.display().to_string()));
            }
        }
    }
    env
}

/// The launcher's PATH plus the user's toolchain bin directories. Service
/// managers start the launcher with a minimal PATH that often omits
/// `~/.cargo/bin`, so plugin setup commands like `cargo build` would fail.
fn plugin_path_env() -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = std::env::split_paths(&current).collect();
    if let Some(home) = dirs::home_dir() {
        let cargo_bin = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"))
            .join("bin");
        for dir in [home.join(".local").join("bin"), cargo_bin] {
            if dir.is_dir() && !paths.contains(&dir) {
                paths.insert(0, dir);
            }
        }
    }
    std::env::join_paths(paths)
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_else(|_| current.to_string_lossy().into_owned())
}

fn toolchain_home(runtime: &PluginRuntime, toolchain: &ToolchainSection) -> PathBuf {
    toolchain
        .home
        .as_ref()
        .map(|home| {
            if home.is_absolute() {
                home.clone()
            } else {
                runtime.root.join(home)
            }
        })
        .unwrap_or_else(|| runtime.dirs.toolchains.join(&toolchain.name))
}

fn expand_runtime_command(
    runtime: &PluginRuntime,
    command: &str,
    extra: Option<&[(&str, String)]>,
) -> String {
    let mut values = vec![
        ("plugin_dir", runtime.root.display().to_string()),
        ("plugin_home", runtime.dirs.portal.display().to_string()),
        (
            "toolchain_root",
            runtime.dirs.toolchains.display().to_string(),
        ),
    ];
    if let Ok(cwd) = std::env::current_dir() {
        values.push(("cwd", cwd.display().to_string()));
    }
    if let Some(extra) = extra {
        values.extend_from_slice(extra);
    }
    expand_command(command, &values)
}

fn current_session_id() -> String {
    std::env::var("PORTAL_SESSION_ID")
        .or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID"))
        .or_else(|_| std::env::var("CODEX_THREAD_ID"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn surface_state_path(runtime: &PluginRuntime) -> PathBuf {
    runtime.dirs.surfaces.join(SURFACE_STATE_FILE)
}

fn surface_state(runtime: &PluginRuntime) -> Result<Option<SurfaceState>> {
    let path = surface_state_path(runtime);
    if !path.is_file() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(serde_json::from_str(&body).with_context(|| {
        format!("failed to parse {}", path.display())
    })?))
}

fn write_surface_state(runtime: &PluginRuntime, state: &SurfaceState) -> Result<()> {
    std::fs::create_dir_all(&runtime.dirs.surfaces)?;
    let path = surface_state_path(runtime);
    std::fs::write(&path, serde_json::to_string_pretty(state)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Health probes get a per-request timeout so a server that accepts the TCP
/// connection but never answers cannot stall startup past its deadline.
fn health_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_default()
}

async fn surface_healthy(state: &SurfaceState) -> bool {
    let path = state.health_path.as_deref().unwrap_or("/");
    let url = format!("http://127.0.0.1:{}{}", state.port, path);
    health_client()
        .get(url)
        .send()
        .await
        .map(|resp| resp.status().is_success())
        .unwrap_or(false)
}

fn run_cmd(cwd: &Path, program: &str, args: &[&OsStr]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{program} exited with {status}")
    }
}

fn expand_command(command: &str, values: &[(&str, String)]) -> String {
    let mut out = command.to_string();
    for (key, value) in values {
        out = out.replace(&format!("{{{key}}}"), &shell_quote(value));
    }
    out
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

async fn wait_for_health(
    exited: &std::sync::OnceLock<std::process::ExitStatus>,
    port: u16,
    health_path: Option<&str>,
) -> Result<()> {
    let path = health_path.unwrap_or("/");
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = health_client();
    let deadline = std::time::Instant::now() + SURFACE_HEALTH_TIMEOUT;
    loop {
        if let Some(status) = exited.get() {
            bail!("plugin surface exited with {status} before becoming healthy");
        }
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "plugin surface did not become healthy at {url} within {}s",
                SURFACE_HEALTH_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn path_display(path: &Path) -> String {
    if path.as_os_str().is_empty() || path == Path::new(".") {
        ".".to_string()
    } else {
        path.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_runtime(root: PathBuf) -> PluginRuntime {
        PluginRuntime {
            name: "kicad-pcb".to_string(),
            root: root.clone(),
            installed: InstalledPlugin {
                path: root.display().to_string(),
                source: "test".to_string(),
                source_subdir: None,
                reference: None,
                enabled: true,
                installed_at: chrono::Utc::now(),
                revision: None,
            },
            manifest: PluginManifest {
                name: "kicad-pcb".to_string(),
                display_name: None,
                description: None,
                homepage: None,
                install: InstallSection::default(),
                surface: None,
                skills: Vec::new(),
                prompts: Vec::new(),
                commands: Vec::new(),
                toolchains: Vec::new(),
                capabilities: std::collections::BTreeMap::new(),
            },
            dirs: PluginRuntimeDirs::new(&root),
        }
    }

    #[test]
    fn plugin_commands_use_plugin_local_home_and_xdg_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("kicad-pcb");
        std::fs::create_dir_all(&root).unwrap();
        let runtime = test_runtime(root.clone());

        prepare_plugin_home(&root).unwrap();

        for name in ["home", "cache", "config", "data", "state", "toolchains"] {
            assert!(root.join(".portal").join(name).is_dir());
        }
        assert!(root.join(".portal/state/surfaces").is_dir());

        let env = plugin_command_env(&runtime);
        let get = |key: &str| {
            env.iter()
                .find_map(|(k, v)| (*k == key).then_some(v.as_str()))
                .unwrap()
        };

        assert_eq!(get("AGENT_PORTAL_PLUGIN_DIR"), root.display().to_string());
        assert_eq!(
            get("AGENT_PORTAL_PLUGIN_HOME"),
            root.join(".portal").display().to_string()
        );
        assert_eq!(
            get("AGENT_PORTAL_PLUGIN_TOOLCHAIN_ROOT"),
            root.join(".portal/toolchains").display().to_string()
        );
        assert_eq!(get("HOME"), root.join(".portal/home").display().to_string());
        assert_eq!(
            get("XDG_CACHE_HOME"),
            root.join(".portal/cache").display().to_string()
        );
        assert_eq!(
            get("XDG_CONFIG_HOME"),
            root.join(".portal/config").display().to_string()
        );
        assert_eq!(
            get("XDG_DATA_HOME"),
            root.join(".portal/data").display().to_string()
        );
        assert_eq!(
            get("XDG_STATE_HOME"),
            root.join(".portal/state").display().to_string()
        );
    }

    #[test]
    fn toolchain_placeholder_expands_to_managed_home() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("kicad-pcb");
        std::fs::create_dir_all(&root).unwrap();
        let runtime = test_runtime(root.clone());
        let toolchain = ToolchainSection {
            name: "kicad".to_string(),
            description: None,
            home: None,
            install: None,
            doctor: None,
            env: std::collections::BTreeMap::new(),
        };

        let command = expand_runtime_command(
            &runtime,
            "echo {toolchain_home} {toolchain_root}",
            Some(&[(
                "toolchain_home",
                toolchain_home(&runtime, &toolchain).display().to_string(),
            )]),
        );

        assert_eq!(
            command,
            format!(
                "echo '{}' '{}'",
                root.join(".portal/toolchains/kicad").display(),
                root.join(".portal/toolchains").display()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn stopping_a_surface_reaps_children_of_the_sh_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("kicad-pcb");
        std::fs::create_dir_all(&root).unwrap();
        let runtime = test_runtime(root.clone());
        let log = root.join("surface.log");
        // The `sh -c` wrapper stays the parent of its server, which is the
        // shape that orphaned servers when only the wrapper was killed. The
        // child also ignores SIGTERM and outlives the leader, so the stop has
        // to keep probing the group and escalate to SIGKILL.
        let mut child = spawn_surface(
            &runtime,
            "(trap '' TERM; exec sleep 300) & echo booted; wait",
            &log,
        )
        .unwrap();
        let pid = child.id();
        let state = SurfaceState {
            plugin: "kicad-pcb".to_string(),
            port: 0,
            pid: Some(pid),
            process_group: true,
            command: String::new(),
            cwd: String::new(),
            session_id: String::new(),
            health_path: None,
            started_at: chrono::Utc::now(),
        };
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while log_tail(&log, 5).is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(surface_alive(&state));

        stop_surface_process(&state).unwrap();

        assert!(!surface_alive(&state), "the surface process group survived");
        assert_eq!(log_tail(&log, 5), vec!["booted".to_string()]);
    }

    #[test]
    fn log_tail_returns_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("surface.log");
        assert!(log_tail(&path, 3).is_empty());
        std::fs::write(&path, "a\nb\nc\nd\n").unwrap();
        assert_eq!(log_tail(&path, 2), vec!["c".to_string(), "d".to_string()]);
        assert_eq!(log_tail(&path, 10).len(), 4);
    }

    #[test]
    fn recovery_after_an_interrupted_update_keeps_the_moved_plugin_state() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("demo");
        let backup = replace_backup_path("demo", &dest);
        // Interrupted after the old `.portal` moved into the new checkout but
        // before setup finished: the backup lacks state, the partial has it.
        std::fs::create_dir_all(backup.join("bin")).unwrap();
        std::fs::write(backup.join("VERSION"), "old").unwrap();
        std::fs::create_dir_all(dest.join(".portal/toolchains/kicad")).unwrap();
        std::fs::write(dest.join("VERSION"), "partial").unwrap();

        recover_interrupted_replace("demo", &dest).unwrap();

        assert!(!backup.exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("VERSION")).unwrap(),
            "old"
        );
        assert!(dest.join(".portal/toolchains/kicad").is_dir());
    }

    #[test]
    fn a_failed_registration_rolls_the_checkout_back() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("VERSION"), "new").unwrap();
        let dest = dir.path().join("demo");
        std::fs::create_dir_all(dest.join(".portal/data")).unwrap();
        std::fs::write(dest.join("VERSION"), "old").unwrap();

        let err = replace_checkout("demo", &source, &dest, None, true, || {
            bail!("config write failed")
        })
        .unwrap_err();

        assert!(format!("{err:#}").contains("previous version was restored"));
        assert_eq!(
            std::fs::read_to_string(dest.join("VERSION")).unwrap(),
            "old"
        );
        assert!(dest.join(".portal/data").is_dir());
        assert!(!replace_backup_path("demo", &dest).exists());
    }
}

//! `agent-portal plugin` subcommands.
//!
//! This is intentionally launcher-local: plugins install into a user-owned
//! checkout directory, run on the same host as the agent session, and reuse the
//! existing `agent-portal forward` path for session surfaces.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{self, InstalledPlugin};

const MANIFEST: &str = "agent-portal-plugin.toml";
const SURFACE_STATE_FILE: &str = "surface.json";

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

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SurfaceState {
    plugin: String,
    port: u16,
    pid: Option<u32>,
    command: String,
    cwd: String,
    session_id: String,
    health_path: Option<String>,
    started_at: chrono::DateTime<chrono::Utc>,
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
        let value = serde_json::json!({
            "name": runtime.name,
            "display_name": runtime.manifest.display_name,
            "description": runtime.manifest.description,
            "path": runtime.root,
            "enabled": runtime.installed.enabled,
            "plugin_home": runtime.dirs.portal,
            "toolchain_root": runtime.dirs.toolchains,
            "surface": runtime.manifest.surface.as_ref().map(|surface| serde_json::json!({
                "default_title": surface.default_title,
                "health_path": surface.health_path,
                "default_width_percent": surface.default_width_percent,
                "has_start": surface.start.is_some(),
                "has_stop": surface.stop.is_some(),
            })),
            "skills": runtime.manifest.skills.iter().map(|skill| serde_json::json!({
                "name": skill.name,
                "path": runtime.root.join(&skill.path),
                "agents": skill.agents,
            })).collect::<Vec<_>>(),
            "commands": runtime.manifest.commands.iter().map(|command| serde_json::json!({
                "name": command.name,
                "description": command.description,
                "run": command.run,
            })).collect::<Vec<_>>(),
            "toolchains": runtime.manifest.toolchains.iter().map(|toolchain| serde_json::json!({
                "name": toolchain.name,
                "description": toolchain.description,
                "home": toolchain_home(&runtime, toolchain),
                "has_install": toolchain.install.is_some(),
                "has_doctor": toolchain.doctor.is_some(),
            })).collect::<Vec<_>>(),
            "capabilities": runtime.manifest.capabilities,
        });
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
            .map(|toolchain| {
                serde_json::json!({
                    "name": toolchain.name,
                    "description": toolchain.description,
                    "home": toolchain_home(&runtime, toolchain),
                    "has_install": toolchain.install.is_some(),
                    "has_doctor": toolchain.doctor.is_some(),
                    "env": toolchain.env,
                })
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

pub fn install(source: &str, name: Option<&str>, reference: Option<&str>) -> Result<()> {
    let spec = parse_source(source)?;
    let staging = materialize_source(&spec, reference)?;
    let source_root = staging.join(&spec.subdir);
    let manifest = load_manifest(&source_root)?;
    let install_name = name.unwrap_or(&manifest.name);
    validate_plugin_name(install_name)?;
    if install_name != manifest.name {
        bail!(
            "install name `{install_name}` does not match manifest name `{}`",
            manifest.name
        );
    }

    let dest = config::plugin_root().join(install_name);
    if dest.exists() {
        bail!(
            "plugin `{install_name}` already exists at {}; remove it first",
            dest.display()
        );
    }
    copy_dir(&source_root, &dest).with_context(|| {
        format!(
            "failed to copy plugin from {} to {}",
            source_root.display(),
            dest.display()
        )
    })?;

    if let Some(setup) = &manifest.install.setup {
        println!("Running setup: {setup}");
        run_manifest_command(&dest, setup)?;
    }

    config::save_installed_plugin(
        install_name,
        InstalledPlugin {
            path: dest.display().to_string(),
            source: spec.original,
            source_subdir: Some(path_display(&spec.subdir)),
            reference: reference.map(str::to_string),
            enabled: true,
            installed_at: chrono::Utc::now(),
        },
    )?;
    println!("Installed {install_name} to {}", dest.display());
    Ok(())
}

pub fn remove(name: &str) -> Result<()> {
    let Some(installed) = config::remove_installed_plugin(name)? else {
        bail!("plugin `{name}` is not installed");
    };
    let path = PathBuf::from(installed.path);
    if path.exists() {
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    println!("Removed {name}.");
    Ok(())
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
    if let Some(state) = surface_state(&runtime)? {
        if surface_healthy(&state).await {
            println!("plugin `{name}` surface already running on {}", state.port);
            return Ok(state);
        }
        let _ = std::fs::remove_file(surface_state_path(&runtime));
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
    let session_id = current_session_id();
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let command = expand_runtime_command(
        &runtime,
        start,
        Some(&[
            ("port", port.to_string()),
            ("session_id", session_id.clone()),
            ("cwd", cwd.display().to_string()),
        ]),
    );
    let child = spawn_runtime_command(&runtime, &command)?;
    let state = SurfaceState {
        plugin: runtime.name.clone(),
        port,
        pid: Some(child.id()),
        command,
        cwd: cwd.display().to_string(),
        session_id,
        health_path: surface.health_path.clone(),
        started_at: chrono::Utc::now(),
    };
    wait_for_health(port, surface.health_path.as_deref()).await?;
    write_surface_state(&runtime, &state)?;
    println!("started `{name}` surface on 127.0.0.1:{port}");
    Ok(state)
}

pub async fn status(name: &str, json: bool) -> Result<()> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    let state = surface_state(&runtime)?;
    let healthy = match &state {
        Some(state) => surface_healthy(state).await,
        None => false,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "plugin": name,
                "surface": state,
                "healthy": healthy,
            }))?
        );
    } else if let Some(state) = state {
        println!(
            "{}\t{}\t127.0.0.1:{}\tpid={}",
            name,
            if healthy { "healthy" } else { "stale" },
            state.port,
            state
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    } else {
        println!("{name}\tstopped");
    }
    Ok(())
}

pub fn stop(name: &str) -> Result<()> {
    let runtime = load_runtime(name)?;
    ensure_enabled(name, &runtime.installed)?;
    let Some(state) = surface_state(&runtime)? else {
        println!("plugin `{name}` surface is not running.");
        return Ok(());
    };
    if let Some(stop) = runtime
        .manifest
        .surface
        .as_ref()
        .and_then(|surface| surface.stop.as_deref())
    {
        run_runtime_command(
            &runtime,
            stop,
            Some(&[
                ("port", state.port.to_string()),
                ("session_id", state.session_id.clone()),
                ("cwd", state.cwd.clone()),
            ]),
        )?;
    } else if let Some(pid) = state.pid {
        #[cfg(unix)]
        {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
    let _ = std::fs::remove_file(surface_state_path(&runtime));
    println!("stopped `{name}` surface.");
    Ok(())
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

fn materialize_source(spec: &SourceSpec, reference: Option<&str>) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "agent-portal-plugin-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
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
                    dir.as_os_str(),
                ],
            )?;
            if let Some(reference) = reference {
                run_cmd(
                    &dir,
                    "git",
                    &[
                        OsStr::new("fetch"),
                        OsStr::new("origin"),
                        OsStr::new(reference),
                    ],
                )?;
                run_cmd(
                    &dir,
                    "git",
                    &[OsStr::new("checkout"), OsStr::new(reference)],
                )?;
            }
        }
        SourceFetch::Local(path) => {
            let path = path
                .canonicalize()
                .with_context(|| format!("failed to resolve {}", path.display()))?;
            copy_dir(&path, &dir)?;
        }
    }
    Ok(dir)
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
    } else {
        bail!("command `{command}` exited with {status}")
    }
}

fn spawn_runtime_command(runtime: &PluginRuntime, command: &str) -> Result<std::process::Child> {
    prepare_plugin_home(&runtime.root)?;
    shell_command(runtime, command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start `{command}`"))
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
    vec![
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
    ]
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

async fn surface_healthy(state: &SurfaceState) -> bool {
    let path = state.health_path.as_deref().unwrap_or("/");
    let url = format!("http://127.0.0.1:{}{}", state.port, path);
    reqwest::Client::new()
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

async fn wait_for_health(port: u16, health_path: Option<&str>) -> Result<()> {
    let path = health_path.unwrap_or("/");
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            bail!("plugin surface did not become healthy at {url}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
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
            name: "backplane".to_string(),
            root: root.clone(),
            installed: InstalledPlugin {
                path: root.display().to_string(),
                source: "test".to_string(),
                source_subdir: None,
                reference: None,
                enabled: true,
                installed_at: chrono::Utc::now(),
            },
            manifest: PluginManifest {
                name: "backplane".to_string(),
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
        let root = dir.path().join("backplane");
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
        let root = dir.path().join("backplane");
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
}

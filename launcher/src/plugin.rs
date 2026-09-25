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
use serde::Deserialize;

use crate::config::{self, InstalledPlugin};

const MANIFEST: &str = "agent-portal-plugin.toml";

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
    health_path: Option<String>,
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
    let installed = installed_plugin(name)?;
    let manifest = load_manifest(Path::new(&installed.path))?;
    println!(
        "{} ({name})",
        manifest.display_name.as_deref().unwrap_or(name)
    );
    if let Some(description) = manifest.description {
        println!("{description}");
    }
    if let Some(homepage) = manifest.homepage {
        println!("homepage: {homepage}");
    }
    println!("path: {}", installed.path);
    println!("source: {}", installed.source);
    println!(
        "source_subdir: {}",
        installed.source_subdir.as_deref().unwrap_or(".")
    );
    println!(
        "status: {}",
        if installed.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    if !manifest.skills.is_empty() {
        println!("skills:");
        for skill in manifest.skills {
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
        for prompt in manifest.prompts {
            println!("  - {} {}", prompt.name, prompt.path.display());
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
    let installed = installed_plugin(name)?;
    ensure_enabled(name, &installed)?;
    let root = PathBuf::from(&installed.path);
    let manifest = load_manifest(&root)?;
    let Some(command) = manifest.install.doctor else {
        bail!("plugin `{name}` does not declare an install.doctor command");
    };
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let command = expand_command(
        &command,
        &[
            ("cwd", cwd.display().to_string()),
            ("plugin_dir", root.display().to_string()),
        ],
    );
    run_manifest_command(&root, &command)
}

pub async fn open(name: &str) -> Result<()> {
    let installed = installed_plugin(name)?;
    ensure_enabled(name, &installed)?;
    let root = PathBuf::from(&installed.path);
    let manifest = load_manifest(&root)?;
    let Some(surface) = manifest.surface else {
        bail!("plugin `{name}` does not declare a surface");
    };
    let Some(start) = surface.start else {
        bail!("plugin `{name}` surface does not declare a start command");
    };
    let port = free_port()?;
    let session_id = std::env::var("PORTAL_SESSION_ID")
        .or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID"))
        .or_else(|_| std::env::var("CODEX_THREAD_ID"))
        .unwrap_or_else(|_| "unknown".to_string());
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let command = expand_command(
        &start,
        &[
            ("port", port.to_string()),
            ("session_id", session_id),
            ("cwd", cwd.display().to_string()),
            ("plugin_dir", root.display().to_string()),
        ],
    );

    spawn_manifest_command(&root, &command)?;
    wait_for_health(port, surface.health_path.as_deref()).await?;
    let title = surface
        .default_title
        .or(manifest.display_name)
        .unwrap_or_else(|| name.to_string());
    eprintln!("Opened {title} on 127.0.0.1:{port}");
    crate::forward::open(port).await
}

fn installed_plugin(name: &str) -> Result<InstalledPlugin> {
    config::load_config()
        .plugins
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow!("plugin `{name}` is not installed"))
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
    prepare_plugin_home(root)?;
    let status = shell_command(root, command)
        .status()
        .with_context(|| format!("failed to run `{command}`"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("command `{command}` exited with {status}")
    }
}

fn spawn_manifest_command(root: &Path, command: &str) -> Result<()> {
    prepare_plugin_home(root)?;
    shell_command(root, command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start `{command}`"))?;
    Ok(())
}

fn prepare_plugin_home(root: &Path) -> Result<()> {
    let portal_dir = root.join(".portal");
    for name in ["home", "cache", "config", "data", "state"] {
        std::fs::create_dir_all(portal_dir.join(name)).with_context(|| {
            format!(
                "failed to prepare plugin support directory {}",
                portal_dir.join(name).display()
            )
        })?;
    }
    Ok(())
}

fn shell_command(root: &Path, command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(root)
        .envs(plugin_command_env(root));
    cmd
}

fn plugin_command_env(root: &Path) -> Vec<(&'static str, String)> {
    let portal_dir = root.join(".portal");
    vec![
        ("AGENT_PORTAL_PLUGIN_DIR", root.display().to_string()),
        ("AGENT_PORTAL_PLUGIN_HOME", portal_dir.display().to_string()),
        ("HOME", portal_dir.join("home").display().to_string()),
        (
            "XDG_CACHE_HOME",
            portal_dir.join("cache").display().to_string(),
        ),
        (
            "XDG_CONFIG_HOME",
            portal_dir.join("config").display().to_string(),
        ),
        (
            "XDG_DATA_HOME",
            portal_dir.join("data").display().to_string(),
        ),
        (
            "XDG_STATE_HOME",
            portal_dir.join("state").display().to_string(),
        ),
    ]
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

    #[test]
    fn plugin_commands_use_plugin_local_home_and_xdg_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("backplane");
        std::fs::create_dir_all(&root).unwrap();

        prepare_plugin_home(&root).unwrap();

        for name in ["home", "cache", "config", "data", "state"] {
            assert!(root.join(".portal").join(name).is_dir());
        }

        let env = plugin_command_env(&root);
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
}

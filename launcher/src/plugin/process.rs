//! Process mechanics shared by setup, doctor, toolchains, and HTTP surfaces.
//! Plugin selection, state-file persistence, and health policy live in the
//! parent module. Command preparation always uses the same cwd and environment;
//! synchronous commands inherit output and surfaces capture it to their log.

use super::{PluginRuntime, PluginRuntimeDirs, SurfaceState};
use anyhow::{bail, Context, Result};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[cfg(unix)]
use std::time::Duration;

pub(super) struct PluginProcess<'a> {
    runtime: &'a PluginRuntime,
}

impl<'a> PluginProcess<'a> {
    pub(super) fn new(runtime: &'a PluginRuntime) -> Self {
        Self { runtime }
    }
    pub(super) fn run(&self, command: &str, extra: Option<&[(&str, String)]>) -> Result<()> {
        let command = self.expand(command, extra);
        let status = self
            .command(&command)?
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
    pub(super) fn spawn_surface(
        &self,
        command: &str,
        log_path: &Path,
    ) -> Result<std::process::Child> {
        let mut cmd = self.command(command)?;
        let log = std::fs::File::create(log_path)
            .with_context(|| format!("failed to create {}", log_path.display()))?;
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

    pub(super) fn expand(&self, command: &str, extra: Option<&[(&str, String)]>) -> String {
        let runtime = self.runtime;
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

    fn command(&self, command: &str) -> Result<Command> {
        let runtime = self.runtime;
        prepare_plugin_home(&runtime.root)?;
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&runtime.root)
            .envs(plugin_command_env(runtime));
        Ok(cmd)
    }
}

/// Whether any process from a recorded surface is still alive. A surface
/// spawned as a group leader is probed as a group, so children that outlive
/// the leader still count.
#[cfg(unix)]
pub(super) fn surface_alive(state: &SurfaceState) -> bool {
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
pub(super) fn surface_alive(state: &SurfaceState) -> bool {
    state.pid.is_some()
}

/// Terminate a surface's process tree: SIGTERM, wait, then SIGKILL. Errors
/// if anything survives, so callers never report a surface stopped while it
/// still holds its port.
#[cfg(unix)]
pub(super) fn stop_surface_process(state: &SurfaceState) -> Result<()> {
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
pub(super) fn stop_surface_process(state: &SurfaceState) -> Result<()> {
    if state.pid.is_some() {
        eprintln!(
            "plugin `{}` declares no stop command; removing surface state only",
            state.plugin
        );
    }
    Ok(())
}

/// Last `lines` lines of a log file (empty if missing).
pub(super) fn log_tail(path: &Path, lines: usize) -> Vec<String> {
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

pub(super) fn prepare_plugin_home(root: &Path) -> Result<()> {
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

pub(super) fn plugin_command_env(runtime: &PluginRuntime) -> Vec<(&'static str, String)> {
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

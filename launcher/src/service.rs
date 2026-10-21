use anyhow::Result;

/// Prepend `dir` to a colon-separated PATH unless already present.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn path_with_dir(existing: &str, dir: &str) -> String {
    if dir.is_empty() || existing.split(':').any(|p| p == dir) {
        existing.to_string()
    } else if existing.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}:{existing}")
    }
}

/// Build a PATH for the service that includes `$HOME/.local/bin` — the default
/// location of the native `claude` installer. Prepends it to the supplied PATH
/// unless already present, so a service installed from a shell that lacked it
/// (or a unit file predating this logic) can still resolve the agent binary.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_with_local_bin(existing: &str, home: Option<&str>) -> String {
    match home {
        Some(h) if !h.is_empty() => path_with_dir(existing, &format!("{h}/.local/bin")),
        _ => existing.to_string(),
    }
}

/// The PATH the installed service should run with: the current environment's
/// PATH, guaranteed to include `$HOME/.local/bin` (for `claude`), an existing
/// `$HOME/.cargo/bin` (plugin setup commands build with `cargo`), and the
/// launcher binary's own directory — agents spawned by the service shell out
/// to `agent-portal` (messaging, port forwarding), and the binary often lives
/// somewhere systemd/launchd's minimal PATH doesn't cover.
#[cfg(target_os = "linux")]
fn service_path(binary_path: &str) -> String {
    let home = std::env::var("HOME").ok();
    let mut base = path_with_local_bin(&std::env::var("PATH").unwrap_or_default(), home.as_deref());
    if let Some(home) = home.filter(|h| shared::strings::is_non_empty(h)) {
        let cargo_bin = format!("{home}/.cargo/bin");
        if std::path::Path::new(&cargo_bin).is_dir() {
            base = path_with_dir(&base, &cargo_bin);
        }
    }
    match std::path::Path::new(binary_path).parent() {
        Some(dir) => path_with_dir(&base, &dir.to_string_lossy()),
        None => base,
    }
}

/// Path of the running executable, with any trailing ` (deleted)` stripped.
///
/// On Linux `std::env::current_exe()` is implemented via `readlink /proc/self/exe`.
/// Per `proc(5)`, if the underlying inode has been unlinked (e.g. by an in-place
/// `agent-portal update` that replaces the binary at the same path), the kernel
/// returns the original path with the literal string ` (deleted)` appended. We
/// strip that suffix so it cannot leak into a generated unit file / plist and
/// produce an `ExecStart` that systemd parses as a bogus subcommand.
pub(crate) fn current_exe_path() -> Result<String> {
    use anyhow::Context;
    Ok(std::env::current_exe()
        .context("Failed to get current executable path")?
        .to_string_lossy()
        .trim_end_matches(" (deleted)")
        .to_string())
}

// --- Linux (systemd) ---

#[cfg(target_os = "linux")]
const SERVICE_NAME: &str = "agent-portal";

#[cfg(target_os = "linux")]
fn service_file_path() -> Result<std::path::PathBuf> {
    use anyhow::Context;
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(std::path::PathBuf::from(home)
        .join(".config/systemd/user")
        .join(format!("{}.service", SERVICE_NAME)))
}

/// The launcher's user unit delegates memory control to per-session leaves.
/// OOM settings also protect the fallback on hosts without cgroup v2 delegation:
/// `OOMPolicy=continue` keeps the service up when the kernel kills a process
/// inside it (systemd's default, `stop`, tears the whole unit down), and
/// `ManagedOOMPreference=avoid` asks `systemd-oomd` to pick other cgroups first.
#[cfg(target_os = "linux")]
fn generate_unit(binary_path: &str) -> String {
    let path = service_path(binary_path);
    format!(
        r"[Unit]
Description=Agent Launcher
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=PATH={path}
ExecStart={binary_path} --no-update
Restart=on-failure
RestartSec=5
OOMPolicy=continue
ManagedOOMPreference=avoid
Delegate=memory

[Install]
WantedBy=default.target
"
    )
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> Result<std::process::Output> {
    use anyhow::Context;
    std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| format!("Failed to run: systemctl --user {}", args.join(" ")))
}

#[cfg(target_os = "linux")]
pub fn install() -> Result<()> {
    use anyhow::Context;
    let binary_path = current_exe_path()?;

    let service_path = service_file_path()?;

    if service_path.exists() {
        println!("Service file already exists at {}", service_path.display());
        println!("Use 'service uninstall' first to reinstall.");
        return Ok(());
    }

    if let Some(parent) = service_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create systemd user directory")?;
    }

    let unit = generate_unit(&binary_path);
    std::fs::write(&service_path, unit)
        .with_context(|| format!("Failed to write {}", service_path.display()))?;

    println!("Wrote {}", service_path.display());

    systemctl(&["daemon-reload"])?;
    println!("Reloaded systemd daemon");

    systemctl(&["enable", SERVICE_NAME])?;
    println!("Enabled {}", SERVICE_NAME);

    systemctl(&["start", SERVICE_NAME])?;
    println!("Started {}", SERVICE_NAME);

    println!();
    println!("Launcher is installed and running.");
    println!("  Logs: journalctl --user -u {} -f", SERVICE_NAME);

    Ok(())
}

/// Regenerate the installed unit file from the current executable + PATH and
/// reload systemd if it changed. Called after an auto-update so a stale unit
/// (e.g. one predating `Environment=PATH`) self-heals. No-op if not installed.
#[cfg(target_os = "linux")]
pub fn sync() -> Result<()> {
    use anyhow::Context;
    let service_path = service_file_path()?;
    if !service_path.exists() {
        return Ok(());
    }

    let binary_path = current_exe_path()?;
    let unit = generate_unit(&binary_path);

    if std::fs::read_to_string(&service_path).unwrap_or_default() == unit {
        return Ok(());
    }

    std::fs::write(&service_path, unit)
        .with_context(|| format!("Failed to write {}", service_path.display()))?;
    systemctl(&["daemon-reload"])?;
    tracing::info!("Regenerated service unit at {}", service_path.display());
    Ok(())
}

/// Sync the installed unit from the daemon itself. The update and restart
/// paths sync too, but from the binary being replaced, so a directive added in
/// a release only reaches the unit once that release is the one running. No
/// restart here; the unit is reloaded and applies in full on the next start.
/// Skipped unless the unit launches this executable, so running another copy
/// (a dev build, a manual run) never repoints the service at it.
#[cfg(target_os = "linux")]
pub fn sync_on_startup() -> Result<()> {
    let Ok(installed) = std::fs::read_to_string(service_file_path()?) else {
        return Ok(());
    };
    if !unit_runs(&installed, &current_exe_path()?) {
        return Ok(());
    }
    sync()
}

/// Whether `unit`'s `ExecStart` launches `binary_path`.
#[cfg(target_os = "linux")]
fn unit_runs(unit: &str, binary_path: &str) -> bool {
    unit.lines()
        .filter_map(|l| l.strip_prefix("ExecStart="))
        .any(|cmd| {
            cmd.strip_prefix(binary_path)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
}

#[cfg(target_os = "linux")]
pub fn uninstall() -> Result<()> {
    use anyhow::Context;
    let service_path = service_file_path()?;

    if !service_path.exists() {
        println!("Service is not installed.");
        return Ok(());
    }

    let _ = systemctl(&["stop", SERVICE_NAME]);
    println!("Stopped {}", SERVICE_NAME);

    let _ = systemctl(&["disable", SERVICE_NAME]);
    println!("Disabled {}", SERVICE_NAME);

    std::fs::remove_file(&service_path)
        .with_context(|| format!("Failed to remove {}", service_path.display()))?;
    println!("Removed {}", service_path.display());

    systemctl(&["daemon-reload"])?;
    println!("Reloaded systemd daemon");

    println!();
    println!("Launcher service uninstalled.");

    Ok(())
}

#[cfg(target_os = "linux")]
pub fn status() -> Result<()> {
    let service_path = service_file_path()?;

    if !service_path.exists() {
        println!("Service is not installed.");
        println!("  Run 'agent-portal service install' to set it up.");
        return Ok(());
    }

    let output = systemctl(&["status", SERVICE_NAME])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stdout.is_empty() {
        print!("{}", stdout);
    }
    if !stderr.is_empty() {
        eprint!("{}", stderr);
    }

    Ok(())
}

#[cfg(target_os = "linux")]
pub fn start() -> Result<()> {
    if !is_installed() {
        anyhow::bail!("Service is not installed. Run 'agent-portal service install' first.");
    }
    systemctl(&["start", SERVICE_NAME])?;
    println!("Started {}", SERVICE_NAME);
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn stop() -> Result<()> {
    if !is_installed() {
        anyhow::bail!("Service is not installed.");
    }
    systemctl(&["stop", SERVICE_NAME])?;
    println!("Stopped {}", SERVICE_NAME);
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn restart() -> Result<()> {
    systemctl(&["restart", SERVICE_NAME])?;
    println!("Restarted {}", SERVICE_NAME);
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn logs(lines: u32, follow: bool) -> Result<()> {
    use anyhow::Context;
    let mut args = vec!["--user", "-u", SERVICE_NAME, "--no-pager"];
    let lines_str = format!("-n{}", lines);
    args.push(&lines_str);
    if follow {
        args.push("-f");
    }
    let status = std::process::Command::new("journalctl")
        .args(&args)
        .status()
        .context("Failed to run journalctl")?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(target_os = "linux")]
pub fn is_installed() -> bool {
    service_file_path().is_ok_and(|p| p.exists())
}

// --- macOS (launchd) ---

#[cfg(target_os = "macos")]
const PLIST_LABEL: &str = "com.agent-portal.launcher";

#[cfg(target_os = "macos")]
fn plist_path() -> Result<std::path::PathBuf> {
    use anyhow::Context;
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(std::path::PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", PLIST_LABEL)))
}

#[cfg(target_os = "macos")]
fn log_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{}/Library/Logs/agent-portal", home)
}

#[cfg(target_os = "macos")]
fn generate_plist(binary_path: &str) -> Result<String> {
    // Preserve the installed PATH rather than importing whichever shell happened
    // to request an update. First install gets a deterministic set of tool paths.
    let plist = plist_path()?;
    let base = if plist.exists() {
        let output = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "EnvironmentVariables.PATH", "raw", "-o", "-"])
            .arg(&plist)
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "Cannot read installed service PATH: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8(output.stdout)?.trim_end().to_owned()
    } else {
        "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned()
    };
    let home = std::env::var("HOME").ok();
    let mut path = path_with_local_bin(&base, home.as_deref());
    if let Some(home) = home {
        path = path_with_dir(&path, &format!("{home}/.cargo/bin"));
    }
    if let Some(dir) = std::path::Path::new(binary_path).parent() {
        path = path_with_dir(&path, &dir.to_string_lossy());
    }
    Ok(render_plist(binary_path, &path, &log_dir()))
}

#[cfg(any(target_os = "macos", test))]
fn render_plist(binary_path: &str, path: &str, log_dir: &str) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
            .replace('\'', "&apos;")
    };
    let binary_path = escape(binary_path);
    let path = escape(path);
    let log_dir = escape(log_dir);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary_path}</string>
        <string>--no-update</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_dir}/stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/stderr.log</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>
    <!-- launchd jobs default to a 256-fd soft limit (shells get ~1M), which a
         long-lived daemon full of sockets can exhaust; on macOS getaddrinfo
         then fails with a misleading "nodename nor servname" error (#1859).
         The fd leak itself is fixed; this keeps any future one survivable. -->
    <key>SoftResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key>
        <integer>4096</integer>
    </dict>
</dict>
</plist>
"#,
        label = "com.agent-portal.launcher",
    )
}

#[cfg(target_os = "macos")]
pub fn install() -> Result<()> {
    use anyhow::{bail, Context};
    let binary_path = current_exe_path()?;

    let plist = plist_path()?;

    if plist.exists() {
        println!("Service file already exists at {}", plist.display());
        println!("Use 'service uninstall' first to reinstall.");
        return Ok(());
    }

    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).context("Failed to create LaunchAgents directory")?;
    }

    // Ensure log directory exists
    std::fs::create_dir_all(log_dir()).context("Failed to create log directory")?;

    let content = generate_plist(&binary_path)?;
    std::fs::write(&plist, content)
        .with_context(|| format!("Failed to write {}", plist.display()))?;

    println!("Wrote {}", plist.display());

    let output = std::process::Command::new("launchctl")
        .args(["load", &plist.to_string_lossy()])
        .output()
        .context("Failed to run launchctl load")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("launchctl load failed: {}", stderr);
    }

    println!("Loaded {}", PLIST_LABEL);
    println!();
    println!("Launcher is installed and running.");
    println!("  Logs: tail -f {}/stdout.log", log_dir());

    Ok(())
}

/// Regenerate the installed plist from the current executable + PATH if it
/// changed. Called after an auto-update so a stale plist self-heals; the
/// subsequent service restart reloads it. No-op if not installed.
#[cfg(target_os = "macos")]
pub fn sync() -> Result<()> {
    use anyhow::Context;
    let plist = plist_path()?;
    if !plist.exists() {
        return Ok(());
    }

    let binary_path = current_exe_path()?;
    let content = generate_plist(&binary_path)?;

    if std::fs::read_to_string(&plist).unwrap_or_default() == content {
        return Ok(());
    }

    std::fs::write(&plist, content)
        .with_context(|| format!("Failed to write {}", plist.display()))?;
    tracing::info!("Regenerated service plist at {}", plist.display());
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> Result<()> {
    use anyhow::Context;
    let plist = plist_path()?;

    if !plist.exists() {
        println!("Service is not installed.");
        return Ok(());
    }

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist.to_string_lossy()])
        .output();
    println!("Unloaded {}", PLIST_LABEL);

    std::fs::remove_file(&plist)
        .with_context(|| format!("Failed to remove {}", plist.display()))?;
    println!("Removed {}", plist.display());

    println!();
    println!("Launcher service uninstalled.");

    Ok(())
}

#[cfg(target_os = "macos")]
pub fn status() -> Result<()> {
    use anyhow::Context;
    let plist = plist_path()?;

    if !plist.exists() {
        println!("Service is not installed.");
        println!("  Run 'agent-portal service install' to set it up.");
        return Ok(());
    }

    if let Ok(status) = std::fs::read_to_string(format!("{}/restart-status.txt", log_dir())) {
        println!("Last restart: {}", status.trim());
    }
    let output = std::process::Command::new("launchctl")
        .args(["list"])
        .output()
        .context("Failed to run launchctl list")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let matching: Vec<&str> = stdout
        .lines()
        .filter(|line| line.split_whitespace().last() == Some(PLIST_LABEL))
        .collect();

    if matching.is_empty() {
        println!("Service is installed but not running.");
        println!("  Start it: launchctl load {}", plist.display());
    } else {
        for line in matching {
            println!("{}", line);
        }
        println!();
        println!("Service is installed and running.");
        println!("  Logs: tail -f {}/stdout.log", log_dir());
    }

    Ok(())
}

#[cfg(target_os = "macos")]
pub fn start() -> Result<()> {
    use anyhow::Context;
    if !is_installed() {
        anyhow::bail!("Service is not installed. Run 'agent-portal service install' first.");
    }
    let plist = plist_path()?;
    let output = std::process::Command::new("/bin/launchctl")
        .args(["load", &plist.to_string_lossy()])
        .output()
        .context("Failed to run launchctl load")?;
    if !output.status.success() {
        anyhow::bail!(
            "launchctl load failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    println!("Started {}", PLIST_LABEL);
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn stop() -> Result<()> {
    use anyhow::Context;
    if !is_installed() {
        anyhow::bail!("Service is not installed.");
    }
    let plist = plist_path()?;
    let output = std::process::Command::new("/bin/launchctl")
        .args(["unload", &plist.to_string_lossy()])
        .output()
        .context("Failed to run launchctl unload")?;
    if !output.status.success() {
        anyhow::bail!(
            "launchctl unload failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    println!("Stopped {}", PLIST_LABEL);
    Ok(())
}

/// Reload from disk in an independent launchd job. A shell forked by the
/// launcher can be killed by bootout with its parent, stranding the service.
/// `submit` makes launchd own the recovery worker instead.
#[cfg(target_os = "macos")]
pub fn restart() -> Result<()> {
    use anyhow::{bail, Context};
    if !is_installed() {
        bail!("Service is not installed.");
    }
    sync()?;
    let plist = plist_path()?;
    let validation = std::process::Command::new("/usr/bin/plutil")
        .arg("-lint")
        .arg(&plist)
        .output()?;
    if !validation.status.success() {
        bail!(
            "Service plist failed validation; launcher left running: {}",
            String::from_utf8_lossy(&validation.stdout)
        );
    }
    // POSIX getuid needs no subprocess or parsing of ambient command output.
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let target = format!("{domain}/{PLIST_LABEL}");
    let helper = format!("{PLIST_LABEL}.restart.{}", uuid::Uuid::new_v4());
    let logs = log_dir();
    std::fs::create_dir_all(&logs)?;
    let status = format!("{logs}/restart-status.txt");
    let log = format!("{logs}/restart.log");
    std::fs::write(
        &status,
        "Restart requested; waiting for launchd recovery worker.\n",
    )?;
    let output = std::process::Command::new("/bin/launchctl")
        .args([
            "submit",
            "-l",
            &helper,
            "-o",
            &log,
            "-e",
            &log,
            "--",
            "/bin/sh",
            "-c",
            include_str!("service_restart_macos.sh"),
            "agent-portal-restart",
            "/bin/launchctl",
            &domain,
            &target,
        ])
        .arg(&plist)
        .arg(&status)
        .arg(&helper)
        .output()
        .context("Failed to submit launchd recovery worker")?;
    if !output.status.success() {
        let error = format!(
            "Restart not scheduled; launcher left running: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(&status, &error)?;
        bail!(error);
    }
    println!("Restart scheduled; progress: {status}; log: {log}");
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn logs(lines: u32, follow: bool) -> Result<()> {
    use anyhow::Context;
    let log_file = format!("{}/stdout.log", log_dir());
    let mut args = vec!["-n".to_string(), lines.to_string()];
    if follow {
        args.push("-f".to_string());
    }
    args.push(log_file);
    let status = std::process::Command::new("tail")
        .args(&args)
        .status()
        .context("Failed to run tail")?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(target_os = "macos")]
pub fn is_installed() -> bool {
    plist_path().is_ok_and(|p| p.exists())
}

// --- Unsupported platforms ---

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn install() -> Result<()> {
    anyhow::bail!("Service installation is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn sync() -> Result<()> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn sync_on_startup() -> Result<()> {
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn uninstall() -> Result<()> {
    anyhow::bail!("Service management is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn status() -> Result<()> {
    anyhow::bail!("Service management is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn start() -> Result<()> {
    anyhow::bail!("Service management is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn stop() -> Result<()> {
    anyhow::bail!("Service management is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn restart() -> Result<()> {
    anyhow::bail!("Service management is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn logs(_lines: u32, _follow: bool) -> Result<()> {
    anyhow::bail!("Service management is not supported on this platform")
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::{path_with_dir, path_with_local_bin};

    #[test]
    fn plist_survives_clean_exit_and_escapes_paths() {
        let plist = super::render_plist("/Users/O'Brien & Co/bin/portal", "/bin:<tools>", "/logs");
        assert!(plist.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(!plist.contains("SuccessfulExit"));
        assert!(plist.contains("O&apos;Brien &amp; Co"));
        assert!(plist.contains("/bin:&lt;tools&gt;"));
    }

    /// Execute the real recovery worker with a controlled launchctl substitute.
    /// Covers delayed bootout completion, command failures and shell-safe paths
    /// without unloading a service or requiring a macOS host.
    #[cfg(unix)]
    #[test]
    fn restart_worker_retries_and_reports_failures() {
        use std::os::unix::fs::PermissionsExt;
        for (mode, success, attempts) in [
            ("retry", true, 3),
            ("fail", false, 30),
            ("kickfail", false, 1),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let ctl = temp.path().join("launchctl");
            std::fs::write(
                &ctl,
                r#"#!/bin/sh
printf '%s\n' "$1" >> "$TEST_ROOT/calls"
case "$1" in
 bootout) exit 1 ;;
 bootstrap)
  count=0
  [ ! -e "$TEST_ROOT/count" ] || count=$(cat "$TEST_ROOT/count")
  count=$((count + 1))
  printf '%s' "$count" > "$TEST_ROOT/count"
  printf '%s' "$3" > "$TEST_ROOT/plist-arg"
  [ "$MODE" != fail ] && { [ "$MODE" != retry ] || [ "$count" -ge 3 ]; }
  ;;
 kickstart) [ "$MODE" != kickfail ] ;;
 remove) exit 0 ;;
esac
"#,
            )
            .unwrap();
            let sleep = temp.path().join("sleep");
            std::fs::write(&sleep, "#!/bin/sh\nexit 0\n").unwrap();
            for executable in [&ctl, &sleep] {
                std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            let status = temp.path().join("status");
            let plist = temp.path().join("a 'quoted' $(touch INJECTED) plist");
            let output = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    include_str!("service_restart_macos.sh"),
                    "restart-test",
                ])
                .arg(&ctl)
                .args(["gui/501", "gui/501/com.agent-portal.launcher"])
                .arg(&plist)
                .arg(&status)
                .arg("recovery-worker")
                .env("TEST_ROOT", temp.path())
                .env("MODE", mode)
                .env("PATH", format!("{}:/usr/bin:/bin", temp.path().display()))
                .current_dir(temp.path())
                .output()
                .unwrap();
            assert_eq!(output.status.success(), success, "{mode}: {output:?}");
            assert_eq!(
                std::fs::read_to_string(temp.path().join("count")).unwrap(),
                attempts.to_string()
            );
            assert_eq!(
                std::fs::read_to_string(temp.path().join("plist-arg")).unwrap(),
                plist.to_str().unwrap()
            );
            assert!(!temp.path().join("INJECTED").exists());
            let message = std::fs::read_to_string(&status).unwrap();
            assert_eq!(message.contains("RESTART FAILED"), !success);
            let calls = std::fs::read_to_string(temp.path().join("calls")).unwrap();
            assert!(calls.starts_with("bootout\nbootstrap\n"));
            assert_eq!(calls.contains("remove"), success);
        }
    }

    #[test]
    fn prepends_local_bin_when_absent() {
        assert_eq!(
            path_with_local_bin("/usr/bin:/bin", Some("/home/u")),
            "/home/u/.local/bin:/usr/bin:/bin"
        );
    }

    #[test]
    fn leaves_path_untouched_when_already_present() {
        let existing = "/usr/bin:/home/u/.local/bin:/bin";
        assert_eq!(path_with_local_bin(existing, Some("/home/u")), existing);
    }

    #[test]
    fn handles_empty_path() {
        assert_eq!(
            path_with_local_bin("", Some("/home/u")),
            "/home/u/.local/bin"
        );
    }

    #[test]
    fn no_home_returns_existing_unchanged() {
        assert_eq!(path_with_local_bin("/usr/bin", None), "/usr/bin");
        assert_eq!(path_with_local_bin("/usr/bin", Some("")), "/usr/bin");
    }

    #[test]
    fn path_with_dir_prepends_and_dedups() {
        assert_eq!(
            path_with_dir("/usr/bin:/bin", "/home/u/.config/claude-portal"),
            "/home/u/.config/claude-portal:/usr/bin:/bin"
        );
        // Already present anywhere → unchanged.
        let existing = "/usr/bin:/home/u/.config/claude-portal:/bin";
        assert_eq!(
            path_with_dir(existing, "/home/u/.config/claude-portal"),
            existing
        );
        assert_eq!(path_with_dir("", "/opt/x"), "/opt/x");
        assert_eq!(path_with_dir("/usr/bin", ""), "/usr/bin");
    }

    #[test]
    fn unit_path_includes_exe_dir() {
        // The generated service PATH must contain the launcher binary's own
        // directory so spawned agents can shell out to `agent-portal` —
        // systemd/launchd minimal PATHs don't cover ad-hoc install locations.
        #[cfg(target_os = "linux")]
        {
            let unit = super::generate_unit("/home/u/.config/claude-portal/agent-portal");
            let path_line = unit
                .lines()
                .find(|l| l.starts_with("Environment=PATH="))
                .expect("unit has a PATH line");
            assert!(
                path_line.contains("/home/u/.config/claude-portal"),
                "unit PATH missing exe dir: {path_line}"
            );
        }
    }

    /// One OOM-killed build must not take every session down with it (#2230).
    #[cfg(target_os = "linux")]
    #[test]
    fn unit_survives_an_oom_kill_inside_it() {
        let unit = super::generate_unit("/home/u/.config/claude-portal/agent-portal");
        let service: Vec<&str> = unit
            .lines()
            .skip_while(|l| *l != "[Service]")
            .take_while(|l| !l.starts_with("[Install]"))
            .collect();
        assert!(service.contains(&"OOMPolicy=continue"), "{unit}");
        assert!(service.contains(&"ManagedOOMPreference=avoid"), "{unit}");
        assert!(service.contains(&"Delegate=memory"), "{unit}");
    }

    /// Startup sync only rewrites a unit that runs this exact executable.
    #[cfg(target_os = "linux")]
    #[test]
    fn startup_sync_only_claims_its_own_unit() {
        let exe = "/home/u/.config/claude-portal/agent-portal";
        let unit = super::generate_unit(exe);
        assert!(super::unit_runs(&unit, exe));
        assert!(super::unit_runs(&format!("ExecStart={exe}"), exe));
        assert!(!super::unit_runs(
            &unit,
            "/home/u/repo/target/debug/agent-portal"
        ));
        assert!(!super::unit_runs(
            &unit,
            "/home/u/.config/claude-portal/agent"
        ));
        assert!(!super::unit_runs("", exe));
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn is_installed() -> bool {
    false
}

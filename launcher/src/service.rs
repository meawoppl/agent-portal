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
/// PATH, guaranteed to include `$HOME/.local/bin` (for `claude`) and the
/// launcher binary's own directory — agents spawned by the service shell out
/// to `agent-portal` (messaging, port forwarding), and the binary often lives
/// somewhere systemd/launchd's minimal PATH doesn't cover.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn service_path(binary_path: &str) -> String {
    let base = path_with_local_bin(
        &std::env::var("PATH").unwrap_or_default(),
        std::env::var("HOME").ok().as_deref(),
    );
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(target_os = "linux")]
fn generate_unit(binary_path: &str) -> String {
    let path = service_path(binary_path);
    format!(
        r#"[Unit]
Description=Agent Launcher
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=PATH={path}
ExecStart={binary_path} --no-update
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#
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
    service_file_path().map(|p| p.exists()).unwrap_or(false)
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
fn generate_plist(binary_path: &str) -> String {
    let log_dir = log_dir();
    let path = service_path(binary_path);
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
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{log_dir}/stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/stderr.log</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>
</dict>
</plist>
"#,
        label = PLIST_LABEL,
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

    let content = generate_plist(&binary_path);
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
    let content = generate_plist(&binary_path);

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

    let output = std::process::Command::new("launchctl")
        .args(["list"])
        .output()
        .context("Failed to run launchctl list")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let matching: Vec<&str> = stdout.lines().filter(|l| l.contains(PLIST_LABEL)).collect();

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
    std::process::Command::new("launchctl")
        .args(["load", &plist.to_string_lossy()])
        .output()
        .context("Failed to run launchctl load")?;
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
    std::process::Command::new("launchctl")
        .args(["unload", &plist.to_string_lossy()])
        .output()
        .context("Failed to run launchctl unload")?;
    println!("Stopped {}", PLIST_LABEL);
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn restart() -> Result<()> {
    use anyhow::Context;
    let plist = plist_path()?;
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist.to_string_lossy()])
        .output();
    std::process::Command::new("launchctl")
        .args(["load", &plist.to_string_lossy()])
        .output()
        .context("Failed to run launchctl load")?;
    println!("Restarted {}", PLIST_LABEL);
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
    plist_path().map(|p| p.exists()).unwrap_or(false)
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
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn is_installed() -> bool {
    false
}

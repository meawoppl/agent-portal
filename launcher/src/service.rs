use anyhow::Result;

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
    let path = std::env::var("PATH").unwrap_or_default();
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
    let binary_path = std::env::current_exe()
        .context("Failed to get current executable path")?
        .to_string_lossy()
        .to_string();

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
pub fn restart() -> Result<()> {
    systemctl(&["restart", SERVICE_NAME])?;
    println!("Restarted {}", SERVICE_NAME);
    Ok(())
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
    let path = std::env::var("PATH").unwrap_or_default();
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
    let binary_path = std::env::current_exe()
        .context("Failed to get current executable path")?
        .to_string_lossy()
        .to_string();

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
pub fn is_installed() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

// --- Unsupported platforms ---

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn install() -> Result<()> {
    anyhow::bail!("Service installation is not supported on this platform")
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
pub fn restart() -> Result<()> {
    anyhow::bail!("Service management is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn is_installed() -> bool {
    false
}

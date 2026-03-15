use anyhow::{Context, Result};
use std::fmt::Write;

/// Collect system info, build info, service status, and logs, then upload to
/// an unlisted paste on dpaste.org. Prints the resulting URL.
pub async fn upload_diagnostics() -> Result<()> {
    println!("Collecting diagnostics...");

    let mut report = String::with_capacity(64 * 1024);

    write_section(&mut report, "Build Info", &build_info());
    write_section(&mut report, "System Info", &system_info());
    write_section(&mut report, "Service Status", &service_status());
    write_section(&mut report, "Config", &config_redacted());
    write_section(&mut report, "Logs (last 1000 lines)", &collect_logs());

    println!("Uploading...");

    let url = upload_to_dpaste(&report).await?;
    println!();
    println!("Diagnostics uploaded:");
    println!("  {}", url);
    Ok(())
}

fn write_section(buf: &mut String, title: &str, content: &str) {
    let _ = writeln!(buf, "=== {} ===", title);
    let _ = writeln!(buf, "{}", content);
    let _ = writeln!(buf);
}

fn build_info() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let binary = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "<unknown>".into());
    let target = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    };
    let arch = std::env::consts::ARCH;

    format!(
        "version: {}\nbinary: {}\ntarget_os: {}\narch: {}",
        version, binary, target, arch
    )
}

fn system_info() -> String {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "<unknown>".into());

    let uname = std::process::Command::new("uname")
        .arg("-a")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "<unavailable>".into());

    format!("hostname: {}\nuname: {}", hostname, uname)
}

fn service_status() -> String {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl")
            .args(["--user", "status", "agent-portal"])
            .output()
            .map(|o| {
                let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr);
                if !stderr.is_empty() {
                    s.push('\n');
                    s.push_str(&stderr);
                }
                s
            })
            .unwrap_or_else(|e| format!("Failed to get status: {}", e))
    }

    #[cfg(target_os = "macos")]
    {
        let installed = crate::service::is_installed();
        if !installed {
            return "Service is not installed.".into();
        }
        std::process::Command::new("launchctl")
            .args(["list"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout
                    .lines()
                    .filter(|l| l.contains("agent-portal"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|e| format!("Failed to get status: {}", e))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        "Service management not supported on this platform.".into()
    }
}

fn config_redacted() -> String {
    let config = crate::config::load_config();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "backend_url: {}",
        config.backend_url.as_deref().unwrap_or("<not set>")
    );
    let _ = writeln!(
        out,
        "auth_token: {}",
        if config.auth_token.is_some() {
            "<redacted>"
        } else {
            "<not set>"
        }
    );
    let _ = writeln!(
        out,
        "name: {}",
        config.name.as_deref().unwrap_or("<not set>")
    );
    let _ = writeln!(out, "sessions: {}", config.sessions.len());
    for s in &config.sessions {
        let _ = writeln!(
            out,
            "  - {} ({:?}) {}",
            s.working_directory,
            s.agent_type,
            s.session_name.as_deref().unwrap_or("")
        );
    }
    out
}

fn collect_logs() -> String {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("journalctl")
            .args(["--user", "-u", "agent-portal", "--no-pager", "-n", "1000"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.trim().is_empty() {
                    "No journal entries found for agent-portal.".into()
                } else {
                    stdout.to_string()
                }
            })
            .unwrap_or_else(|e| format!("Failed to read journalctl: {}", e))
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let log_dir = format!("{}/Library/Logs/agent-portal", home);
        let stdout_path = format!("{}/stdout.log", log_dir);
        let stderr_path = format!("{}/stderr.log", log_dir);

        let mut out = String::new();

        let _ = writeln!(out, "--- stdout.log ---");
        match tail_file(&stdout_path, 1000) {
            Ok(content) => out.push_str(&content),
            Err(e) => {
                let _ = writeln!(out, "Could not read {}: {}", stdout_path, e);
            }
        }

        let _ = writeln!(out, "\n--- stderr.log ---");
        match tail_file(&stderr_path, 1000) {
            Ok(content) => out.push_str(&content),
            Err(e) => {
                let _ = writeln!(out, "Could not read {}: {}", stderr_path, e);
            }
        }

        out
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        "Log collection not supported on this platform.".into()
    }
}

#[cfg(target_os = "macos")]
fn tail_file(path: &str, lines: usize) -> Result<String> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {}", path))?;
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(lines);
    Ok(all_lines[start..].join("\n"))
}

async fn upload_to_dpaste(content: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://dpaste.org/api/")
        .form(&[
            ("content", content),
            ("syntax", "text"),
            ("expiry_days", "30"),
        ])
        .send()
        .await
        .context("Failed to upload to dpaste.org")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("dpaste.org returned {}: {}", status, body);
    }

    let url = resp.text().await.context("Failed to read response")?;
    Ok(url.trim().to_string())
}

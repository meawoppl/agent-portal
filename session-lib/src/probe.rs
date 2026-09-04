//! Synchronous PATH-resolution + `--version` probes for the agent binaries
//! the launcher can spawn. Used at launcher startup (sent in the register
//! envelope) and on demand (refreshed when the user opens the launch dialog).

use shared::AgentType;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Result of probing one agent binary on the host.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub installed: bool,
    pub resolved_path: Option<PathBuf>,
    pub version: Option<String>,
    /// Sandbox readiness for agents whose tool execution depends on host
    /// packages. `None` for agents with no sandbox concept.
    pub sandbox_ok: Option<bool>,
}

/// Probe both supported agent CLIs. Cheap — each binary returns from
/// `--version` in tens of milliseconds.
pub fn probe_all_agents() -> Vec<(AgentType, ProbeResult)> {
    [AgentType::Claude, AgentType::Codex, AgentType::Muse]
        .into_iter()
        .map(|agent| (agent, probe_agent(agent)))
        .collect()
}

/// Probe one agent. Returns the resolved binary path (via `which`) and the
/// `--version` output trimmed. `installed` is true iff `--version` exited 0.
pub fn probe_agent(agent: AgentType) -> ProbeResult {
    let name = agent.as_str();

    let resolved_path = which::which(name).ok();
    if resolved_path.is_none() {
        return ProbeResult {
            installed: false,
            resolved_path: None,
            version: None,
            sandbox_ok: None,
        };
    }

    // Run with a short timeout so a misbehaving binary can't wedge the probe.
    // We use std::process::Command synchronously here because the caller is
    // a one-shot blocking probe at startup / on user demand — no async
    // context required.
    let version = match Command::new(name).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Ok(_) | Err(_) => None,
    };

    let installed = version.is_some();
    ProbeResult {
        installed,
        resolved_path,
        version,
        sandbox_ok: if installed && agent == AgentType::Muse {
            probe_muse_sandbox()
        } else {
            None
        },
    }
}

/// Muse executes tools inside an OS sandbox. Without it, runs still
/// complete but every tool call comes back as a failed `tool.result` — an
/// installed-but-degraded state the matrix must show distinctly from "not
/// installed".
///
/// Returns `None` where this crate cannot honestly attest to sandbox state
/// (rather than claiming ready), so the matrix shows no sandbox indicator
/// instead of a green badge it can't back up.
///
/// - **Linux**: bubblewrap. Probed by resolving `bwrap` on PATH — cheap and
///   side-effect-free (`muse sandbox` exposes no Linux check at 0.1.0).
/// - **Windows**: `muse sandbox windows check` reports a real backend and
///   status (`status=setup_required` is exactly the degraded state), so it
///   is parsed. Written from the observed key=value output format; not yet
///   exercised on a Windows host.
/// - **macOS**: no probe — Muse supports macOS but exposes no sandbox
///   check, and this crate will not assert readiness it cannot verify.
pub fn probe_muse_sandbox() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        Some(which::which("bwrap").is_ok())
    }
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("muse")
            .args(["sandbox", "windows", "check"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let status = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("status="))?;
        // The healthy string is a GUESS: `status=setup_required` was observed
        // on an unconfigured host, but no ready value has been seen. Log the
        // raw line so the first Windows user can confirm or correct this
        // match instead of silently getting a wrong cell.
        tracing::info!(
            status = %status,
            "muse sandbox windows check status (healthy-value match is unverified; report this \
             line if the sandbox is configured but the matrix shows degraded)"
        );
        Some(status == "ready" || status == "ok")
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Probe the `gh` CLI: presence via `gh --version`, sign-in via
/// `gh auth status` (exit 0 iff some host is authenticated). Drives the
/// visual-PR host picker — generation and PR listing run through the host's
/// own `gh`, so an unauthenticated host is not a candidate.
pub fn probe_gh() -> shared::GhStatus {
    let version = match Command::new("gh").arg("--version").output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty()),
        Ok(_) | Err(_) => None,
    };
    let installed = version.is_some();
    let authenticated = installed
        && Command::new("gh")
            .args(["auth", "status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    shared::GhStatus {
        installed,
        authenticated,
        version,
    }
}

/// Presence-only login probe for Muse: the CLI persists no account
/// identity (no `whoami` at 0.1.0), so a logged-in cell carries a provider
/// label instead of a name, annotated when the credential comes from the
/// environment rather than the saved file.
pub fn probe_muse_login() -> shared::AgentLoginStatus {
    let via_env = std::env::var("META_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if via_env {
        return shared::AgentLoginStatus::LoggedIn {
            label: Some("meta".to_string()),
            plan: None,
            via: Some("env".to_string()),
        };
    }
    if muse_codes::auth::credentials_present() {
        shared::AgentLoginStatus::LoggedIn {
            label: Some("meta".to_string()),
            plan: None,
            via: None,
        }
    } else {
        shared::AgentLoginStatus::LoggedOut
    }
}

/// A maximum bound on how long a `probe_all_agents` call should take. Surfaced
/// here so callers don't have to guess at the timeout for the request/response
/// round-trip when probing via WS.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod muse_probe_tests {
    use super::*;

    /// Muse joins the probe set — the matrix needs a column for it even on
    /// hosts where the binary is absent.
    #[test]
    fn probe_covers_all_three_agents() {
        let probed: Vec<AgentType> = probe_all_agents().into_iter().map(|(a, _)| a).collect();
        assert!(probed.contains(&AgentType::Muse));
        assert_eq!(probed.len(), 3);
    }

    /// sandbox_ok is muse-only: claude/codex have no sandbox concept and
    /// must serialize exactly as before (None => field omitted).
    #[test]
    fn sandbox_ok_is_none_for_non_muse_agents() {
        for (agent, result) in probe_all_agents() {
            if agent != AgentType::Muse {
                assert_eq!(
                    result.sandbox_ok, None,
                    "{agent:?} should have no sandbox state"
                );
            }
        }
    }

    /// An absent binary reports not-installed with no sandbox claim —
    /// never `Some(false)`, which would read as "installed but degraded".
    #[test]
    fn missing_binary_makes_no_sandbox_claim() {
        let r = probe_agent(AgentType::Muse);
        if !r.installed {
            assert_eq!(r.sandbox_ok, None);
        }
    }

    /// Presence-only login shape: muse carries a provider label, never a
    /// plan, and marks env-supplied credentials.
    #[test]
    fn muse_login_probe_shape() {
        match probe_muse_login() {
            shared::AgentLoginStatus::LoggedIn { label, plan, via } => {
                assert_eq!(label.as_deref(), Some("meta"));
                assert_eq!(plan, None, "muse exposes no plan/subscription");
                assert!(via.is_none() || via.as_deref() == Some("env"));
            }
            shared::AgentLoginStatus::LoggedOut => {}
            shared::AgentLoginStatus::Unknown => {
                panic!("probe should decide presence, not return Unknown")
            }
        }
    }
}

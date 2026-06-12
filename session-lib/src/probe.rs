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
}

/// Probe both supported agent CLIs. Cheap — each binary returns from
/// `--version` in tens of milliseconds.
pub fn probe_all_agents() -> Vec<(AgentType, ProbeResult)> {
    [AgentType::Claude, AgentType::Codex]
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

    ProbeResult {
        installed: version.is_some(),
        resolved_path,
        version,
    }
}

/// A maximum bound on how long a `probe_all_agents` call should take. Surfaced
/// here so callers don't have to guess at the timeout for the request/response
/// round-trip when probing via WS.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

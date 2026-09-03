//! Best-effort install of an agent CLI on the launcher host.
//!
//! Runs the agent's `AgentType::install_command` (a vendor installer script or
//! a global npm install, per agent) synchronously and reports whether it exited
//! cleanly, surfacing the command's own output tail on failure so the user sees
//! why — an installer error, the program not on PATH, a permissions problem.
//! The launcher invokes this under `spawn_blocking`, exactly like
//! [`crate::probe`].

use shared::AgentType;
use std::process::Command;

/// Outcome of an install attempt.
#[derive(Debug, Clone)]
pub struct InstallResult {
    pub success: bool,
    /// The CLI's own output tail on failure (or a "could not run" reason).
    /// `None` on success.
    pub message: Option<String>,
}

/// Longest error tail to relay to the browser, so a noisy npm failure doesn't
/// balloon a WS message.
const MESSAGE_TAIL: usize = 2000;

/// Run `agent`'s install command and report the outcome. The program/args come
/// from [`AgentType::install_command`] and are spawned directly (never a
/// shell), so there is nothing to escape.
pub fn install_agent(agent: AgentType) -> InstallResult {
    let cmd = agent.install_command();
    match Command::new(cmd.program).args(&cmd.args).output() {
        Ok(output) if output.status.success() => InstallResult {
            success: true,
            message: None,
        },
        Ok(output) => InstallResult {
            success: false,
            message: Some(failure_detail(&output)),
        },
        Err(e) => InstallResult {
            success: false,
            message: Some(format!(
                "could not run `{}`: {e} — is it installed and on PATH?",
                cmd.program
            )),
        },
    }
}

/// Build a human error from a non-zero exit: prefer stderr, fall back to
/// stdout, tail-truncated.
fn failure_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let body = if stderr.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        stderr
    };
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return format!("install command exited with {}", output.status);
    }
    tail(trimmed, MESSAGE_TAIL)
}

/// Last `max` chars of `s`, on a char boundary, prefixed with an ellipsis when
/// truncated.
fn tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let start = s.chars().count() - max;
    let tail: String = s.chars().skip(start).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_short_strings_whole() {
        assert_eq!(tail("hello", 2000), "hello");
    }

    #[test]
    fn tail_truncates_with_an_ellipsis() {
        let long = "x".repeat(2100);
        let out = tail(&long, 2000);
        assert!(out.starts_with('…'));
        assert_eq!(out.chars().count(), 2001); // ellipsis + 2000
    }

    #[test]
    fn tail_respects_char_boundaries() {
        let s = "é".repeat(2100);
        let out = tail(&s, 2000);
        // No panic, and the multibyte chars survive intact.
        assert_eq!(out.chars().filter(|c| *c == 'é').count(), 2000);
    }
}

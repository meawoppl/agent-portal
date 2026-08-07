//! Spawn the `claude` CLI for a session.
//!
//! Spawns `claude --print --verbose --output-format stream-json
//! --input-format stream-json --permission-prompt-tool stdio
//! --replay-user-messages [--prompt-suggestions true]
//! [--session-id <id> | --resume <id> |
//! --resume <source> --fork-session --session-id <new>] [extra...]`
//! and wraps its handles in a [`ClaudeAsyncClient`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio::process::Command;

use claude_codes::AsyncClient as ClaudeAsyncClient;
use session_lib::error::SessionError;
use session_lib::snapshot::SessionConfig;

/// Build the argument list for the `claude` CLI (everything after the binary
/// path). Shared by the library spawn path and the proxy's shim mode so flag
/// changes can't drift between the two.
pub fn claude_cli_args(
    session_id: uuid::Uuid,
    resume: bool,
    fork_from: Option<uuid::Uuid>,
    prompt_suggestions: bool,
    extra_args: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = [
        "--print",
        "--verbose",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
        "--replay-user-messages",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    if prompt_suggestions {
        args.extend(["--prompt-suggestions".to_string(), "true".to_string()]);
    }

    if let Some(source) = (!resume).then_some(fork_from).flatten() {
        // claude-codes exposes ClaudeCliBuilder::fork_from, but that builder
        // cannot carry the portal's arbitrary user extra_args. Keep the SDK's
        // documented flag recipe here until its builder supports passthrough;
        // this function also remains the single argv seam shared with shim mode.
        args.extend([
            "--resume".to_string(),
            source.to_string(),
            "--fork-session".to_string(),
            "--session-id".to_string(),
            session_id.to_string(),
        ]);
    } else if resume {
        args.push("--resume".to_string());
        args.push(session_id.to_string());
    } else {
        args.push("--session-id".to_string());
        args.push(session_id.to_string());
    }

    args.extend(extra_args.iter().cloned());
    args
}

/// Spawn the Claude process and return its async client.
/// Spawn the `claude` CLI and return the client plus the OS process id (when
/// available). The pid lets `Session::stop` signal the agent's process group
/// directly, rather than relying solely on `kill_on_drop` (which the SDK's
/// detached-task ownership of the `Child` defeats — see #927).
pub(crate) async fn spawn_claude(
    config: &SessionConfig,
) -> Result<(ClaudeAsyncClient, Option<u32>), SessionError> {
    let claude_path = config.claude_path.as_deref().unwrap_or(Path::new("claude"));

    log_claude_info(claude_path);

    let args = claude_cli_args(
        config.session_id,
        config.resume,
        config.fork_from_session_id,
        claude_supports_prompt_suggestions(claude_path),
        &config.extra_args,
    );

    let mut cmd = Command::new(claude_path);
    cmd.args(&args);
    cmd.current_dir(&config.working_directory);
    // Claude exports `CLAUDE_CODE_SESSION_ID` to tools it spawns. This is the
    // id passed here initially, but it is process-environment state and can go
    // stale when `/clear` rolls Claude to another conversation. The launcher
    // CLI therefore treats an explicit `PORTAL_SESSION_ID` as authoritative.

    // Log the full command for diagnostics.
    tracing::info!(
        "Spawning Claude: {} {}",
        claude_path.to_string_lossy(),
        args.join(" ")
    );

    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Kill the child when its `Child` handle drops. The I/O task owns the
        // client (and thus the child); when the task is aborted on stop/drop,
        // this guarantees the claude process is reaped rather than orphaned and
        // left holding its transcript and a WebSocket open.
        .kill_on_drop(true);

    // Put the agent in its own process group so `Session::stop` can signal the
    // whole tree (claude + tools it spawns), not just the immediate PID. We
    // can't rely on `kill_on_drop` alone: the SDK's `AsyncClient` keeps the
    // `Child` alive in detached internal tasks, so an aborted I/O task never
    // drops it and the claude process is orphaned (#927).
    #[cfg(unix)]
    cmd.process_group(0);

    let child = cmd.spawn().map_err(SessionError::SpawnFailed)?;
    let pid = child.id();

    let client = ClaudeAsyncClient::new(child).map_err(|e| {
        SessionError::CommunicationError(format!("Failed to create ClaudeAsyncClient: {}", e))
    })?;
    Ok((client, pid))
}

/// Probe the installed CLI once per resolved path before using the additive
/// prompt-suggestion flag. Older fleet hosts reject unknown flags at startup,
/// so capability detection must happen before argv construction.
pub fn claude_supports_prompt_suggestions(claude_path: &Path) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut cache) = cache.lock() else {
        return false;
    };
    if let Some(supported) = cache.get(claude_path).copied() {
        return supported;
    }
    let supported = std::process::Command::new(claude_path)
        .arg("--help")
        .output()
        .ok()
        .is_some_and(|output| {
            String::from_utf8_lossy(&output.stdout).contains("--prompt-suggestions")
                || String::from_utf8_lossy(&output.stderr).contains("--prompt-suggestions")
        });
    cache.insert(claude_path.to_path_buf(), supported);
    supported
}

/// Log the resolved path and version of the claude binary for diagnostics.
fn log_claude_info(claude_path: &Path) {
    if let Ok(full_path) = which::which(claude_path) {
        tracing::info!("Claude binary: {}", full_path.display());
    } else {
        tracing::warn!(
            "Could not resolve full path for '{}' — using PATH lookup",
            claude_path.display()
        );
    }

    match std::process::Command::new(claude_path)
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            tracing::info!("Claude version: {}", version.trim());
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("claude --version failed: {}", stderr.trim());
        }
        Err(e) => {
            tracing::warn!("Failed to run claude --version: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_args_keep_source_and_new_identity_distinct() {
        let source = uuid::Uuid::from_u128(1);
        let new_id = uuid::Uuid::from_u128(2);
        let args = claude_cli_args(
            new_id,
            false,
            Some(source),
            false,
            &["--model".into(), "opus".into()],
        );
        let expected = vec![
            "--resume".to_string(),
            source.to_string(),
            "--fork-session".to_string(),
            "--session-id".to_string(),
            new_id.to_string(),
            "--model".to_string(),
            "opus".to_string(),
        ];
        assert_eq!(&args[args.len() - expected.len()..], expected.as_slice());
    }

    #[test]
    fn resume_ignores_persisted_fork_source() {
        let source = uuid::Uuid::from_u128(1);
        let own_id = uuid::Uuid::from_u128(2);
        let args = claude_cli_args(own_id, true, Some(source), false, &[]);
        assert_eq!(&args[args.len() - 2..], ["--resume", &own_id.to_string()]);
        assert!(!args.iter().any(|arg| arg == "--fork-session"));
    }

    #[test]
    fn fresh_non_fork_launch_uses_new_session_id() {
        let own_id = uuid::Uuid::from_u128(2);
        let args = claude_cli_args(own_id, false, None, false, &[]);
        assert_eq!(
            &args[args.len() - 2..],
            ["--session-id", &own_id.to_string()]
        );
        assert!(!args.iter().any(|arg| arg == "--fork-session"));
    }

    #[test]
    fn enables_typed_prompt_suggestion_frames() {
        let args = claude_cli_args(uuid::Uuid::nil(), false, None, true, &[]);
        let flag = args
            .iter()
            .position(|arg| arg == "--prompt-suggestions")
            .expect("prompt suggestions flag");
        assert_eq!(args.get(flag + 1).map(String::as_str), Some("true"));
    }

    #[test]
    fn omits_prompt_suggestion_flag_for_older_claude() {
        let args = claude_cli_args(uuid::Uuid::nil(), false, false, &[]);
        assert!(!args.iter().any(|arg| arg == "--prompt-suggestions"));
    }
}

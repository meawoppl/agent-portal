//! Spawn the `claude` CLI for a session.
//!
//! Spawns `claude --print --verbose --output-format stream-json
//! --input-format stream-json --permission-prompt-tool stdio
//! --replay-user-messages [--session-id <id> | --resume <id>] [extra...]`
//! and wraps its handles in a [`ClaudeAsyncClient`].

use std::path::Path;
use tokio::process::Command;

use claude_codes::AsyncClient as ClaudeAsyncClient;
use session_lib::error::SessionError;
use session_lib::snapshot::SessionConfig;

/// Build the argument list for the `claude` CLI (everything after the binary
/// path). Shared by the library spawn path and the proxy's shim mode so flag
/// changes can't drift between the two.
pub fn claude_cli_args(session_id: uuid::Uuid, resume: bool, extra_args: &[String]) -> Vec<String> {
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

    if resume {
        args.push("--resume".to_string());
    } else {
        args.push("--session-id".to_string());
    }
    args.push(session_id.to_string());

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

    let args = claude_cli_args(config.session_id, config.resume, &config.extra_args);

    let mut cmd = Command::new(claude_path);
    cmd.args(&args);
    cmd.current_dir(&config.working_directory);
    // Note: no need to inject a session-id env var — Claude Code already
    // exports `CLAUDE_CODE_SESSION_ID` (equal to the `--session-id` we pass) to
    // the tools it spawns, which `agent-portal message` reads for attribution.

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

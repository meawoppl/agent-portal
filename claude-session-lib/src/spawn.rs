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

/// Spawn the Claude process and return its async client.
pub(crate) async fn spawn_claude(
    config: &SessionConfig,
) -> Result<ClaudeAsyncClient, SessionError> {
    let claude_path = config.claude_path.as_deref().unwrap_or(Path::new("claude"));

    log_claude_info(claude_path);

    let mut cmd = Command::new(claude_path);
    cmd.arg("--print")
        .arg("--verbose")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--permission-prompt-tool")
        .arg("stdio")
        .arg("--replay-user-messages");

    if config.resume {
        cmd.arg("--resume").arg(config.session_id.to_string());
    } else {
        cmd.arg("--session-id").arg(config.session_id.to_string());
    }

    for arg in &config.extra_args {
        cmd.arg(arg);
    }

    cmd.current_dir(&config.working_directory);

    // Log the full command for diagnostics.
    let args: Vec<_> = std::iter::once(claude_path.to_string_lossy().to_string())
        .chain(
            [
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
            .map(|s| s.to_string()),
        )
        .chain(if config.resume {
            vec!["--resume".to_string(), config.session_id.to_string()]
        } else {
            vec!["--session-id".to_string(), config.session_id.to_string()]
        })
        .chain(config.extra_args.iter().cloned())
        .collect();
    tracing::info!("Spawning Claude: {}", args.join(" "));

    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = cmd.spawn().map_err(SessionError::SpawnFailed)?;

    ClaudeAsyncClient::new(child).map_err(|e| {
        SessionError::CommunicationError(format!("Failed to create ClaudeAsyncClient: {}", e))
    })
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

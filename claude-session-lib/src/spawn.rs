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
use std::sync::OnceLock;
use std::time::Duration;
use tokio::process::Command;

use claude_codes::AsyncClient as ClaudeAsyncClient;
use session_lib::error::SessionError;
use session_lib::snapshot::SessionConfig;

/// Build the argument list for the `claude` CLI (everything after the binary
/// path). Shared by the library spawn path and the proxy's shim mode so flag
/// changes can't drift between the two.
/// Build the `claude` argv.
///
/// `conversation_id` is claude's own current conversation, when it has diverged
/// from the portal's `session_id` — which `/clear` causes. It is what `--resume`
/// and a fork's source must key on: resuming the portal id after a clear
/// re-opens the pre-clear transcript, and forking it branches from that stale
/// history rather than from what the user can see. `--session-id` still gets the
/// portal id, since that is the identity the backend and the dashboard hold.
pub fn claude_cli_args(
    session_id: uuid::Uuid,
    resume: bool,
    conversation_id: Option<uuid::Uuid>,
    fork_from: Option<uuid::Uuid>,
    prompt_suggestions: bool,
    extra_args: &[String],
) -> Vec<String> {
    // Where claude's history actually lives right now. Shared with the
    // launcher's transcript-existence gates so the file they check is always the
    // file this resumes.
    let transcript_id = crate::transcript::claude_transcript_id(session_id, conversation_id);
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
        // `source` is the source session's conversation id, resolved
        // launcher-side — not its portal id, for the same reason as below.
        args.extend([
            "--resume".to_string(),
            source.to_string(),
            "--fork-session".to_string(),
            "--session-id".to_string(),
            session_id.to_string(),
        ]);
    } else if resume {
        args.push("--resume".to_string());
        args.push(transcript_id.to_string());
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
        config.claude_conversation_id,
        config
            .claude_fork_from_conversation_id
            .or(config.fork_from_session_id),
        claude_supports_prompt_suggestions(claude_path).await,
        &config.extra_args,
    );

    let mut cmd = build_claude_command(claude_path, &args, config);

    // Log the full command for diagnostics.
    tracing::info!(
        "Spawning Claude: {} {}",
        claude_path.to_string_lossy(),
        args.join(" ")
    );

    let child = cmd.spawn().map_err(SessionError::SpawnFailed)?;
    let pid = child.id();

    let client = ClaudeAsyncClient::new(child).map_err(|e| {
        SessionError::CommunicationError(format!("Failed to create ClaudeAsyncClient: {}", e))
    })?;
    Ok((client, pid))
}

/// Build the fully-configured `claude` [`Command`], short of spawning it.
/// Factored out of [`spawn_claude`] so the environment and stdio wiring are
/// assertable in tests without launching a process.
fn build_claude_command(claude_path: &Path, args: &[String], config: &SessionConfig) -> Command {
    let mut cmd = Command::new(claude_path);
    cmd.args(args);
    cmd.current_dir(&config.working_directory);
    // Claude exports `CLAUDE_CODE_SESSION_ID` to the tools it spawns, but that
    // is process-environment state naming claude's *conversation*: `/clear`
    // rolls it onto an id that is not a portal session at all. Export the portal
    // id explicitly — `launcher::message` treats it as authoritative and only
    // falls back to matching on host + working directory, which is ambiguous the
    // moment two sessions share a directory. Codex and Muse already do this.
    cmd.env("PORTAL_SESSION_ID", config.session_id.to_string());

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

    cmd
}

/// Probe the installed CLI once per resolved path before using the additive
/// prompt-suggestion flag. Older fleet hosts reject unknown flags at startup,
/// so capability detection must happen before argv construction.
pub async fn claude_supports_prompt_suggestions(claude_path: &Path) -> bool {
    static CACHE: OnceLock<tokio::sync::Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut cache = cache.lock().await;
    if let Some(supported) = cache.get(claude_path).copied() {
        return supported;
    }

    // A missing or wedged binary must not stall session launch. Transient
    // failures fail open (omit the additive flag) and are not cached, so a
    // later launch can probe again after host pressure recovers.
    let mut probe = Command::new(claude_path);
    probe
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(Duration::from_secs(2), probe.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            tracing::warn!("Claude prompt-suggestion capability probe failed: {error}");
            return false;
        }
        Err(_) => {
            tracing::warn!("Claude prompt-suggestion capability probe timed out");
            return false;
        }
    };
    let supported = String::from_utf8_lossy(&output.stdout).contains("--prompt-suggestions")
        || String::from_utf8_lossy(&output.stderr).contains("--prompt-suggestions");
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
            None,
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

    /// The bug this exists to prevent: `/clear` rolls claude onto a new
    /// conversation, so resuming the portal session id re-opens the *pre-clear*
    /// transcript and silently discards everything since.
    #[test]
    fn resume_uses_claude_conversation_once_it_has_diverged() {
        let portal_id = uuid::Uuid::from_u128(2);
        let after_clear = uuid::Uuid::from_u128(99);
        let args = claude_cli_args(portal_id, true, Some(after_clear), None, false, &[]);

        assert_eq!(
            &args[args.len() - 2..],
            ["--resume", &after_clear.to_string()],
            "resume must open the live conversation, not the portal id"
        );
        assert!(
            !args.contains(&portal_id.to_string()),
            "the pre-clear transcript id must not appear at all"
        );
    }

    /// The overwhelmingly common case: never cleared, so the portal id still
    /// names claude's conversation and nothing changes.
    #[test]
    fn resume_falls_back_to_session_id_when_not_diverged() {
        let portal_id = uuid::Uuid::from_u128(2);
        let args = claude_cli_args(portal_id, true, None, None, false, &[]);
        assert_eq!(
            &args[args.len() - 2..],
            ["--resume", &portal_id.to_string()]
        );
    }

    /// A fork's source is resolved to the source's conversation by the caller,
    /// for the same reason: forking a cleared session by its portal id branches
    /// from history the user can no longer see. The new session still gets the
    /// portal id as its own `--session-id`.
    #[test]
    fn fork_source_is_taken_verbatim_and_new_identity_is_the_portal_id() {
        let new_portal_id = uuid::Uuid::from_u128(2);
        let source_conversation = uuid::Uuid::from_u128(77);
        let args = claude_cli_args(
            new_portal_id,
            false,
            None,
            Some(source_conversation),
            false,
            &[],
        );

        let resume_at = args.iter().position(|a| a == "--resume").expect("--resume");
        assert_eq!(args[resume_at + 1], source_conversation.to_string());
        let session_at = args
            .iter()
            .position(|a| a == "--session-id")
            .expect("--session-id");
        assert_eq!(args[session_at + 1], new_portal_id.to_string());
    }

    #[test]
    fn resume_ignores_persisted_fork_source() {
        let source = uuid::Uuid::from_u128(1);
        let own_id = uuid::Uuid::from_u128(2);
        let args = claude_cli_args(own_id, true, None, Some(source), false, &[]);
        assert_eq!(&args[args.len() - 2..], ["--resume", &own_id.to_string()]);
        assert!(!args.iter().any(|arg| arg == "--fork-session"));
    }

    #[test]
    fn fresh_non_fork_launch_uses_new_session_id() {
        let own_id = uuid::Uuid::from_u128(2);
        let args = claude_cli_args(own_id, false, None, None, false, &[]);
        assert_eq!(
            &args[args.len() - 2..],
            ["--session-id", &own_id.to_string()]
        );
        assert!(!args.iter().any(|arg| arg == "--fork-session"));
    }

    #[test]
    fn enables_typed_prompt_suggestion_frames() {
        let args = claude_cli_args(uuid::Uuid::nil(), false, None, None, true, &[]);
        let flag = args
            .iter()
            .position(|arg| arg == "--prompt-suggestions")
            .expect("prompt suggestions flag");
        assert_eq!(args.get(flag + 1).map(String::as_str), Some("true"));
    }

    #[test]
    fn omits_prompt_suggestion_flag_for_older_claude() {
        let args = claude_cli_args(uuid::Uuid::nil(), false, None, None, false, &[]);
        assert!(!args.iter().any(|arg| arg == "--prompt-suggestions"));
    }

    /// The property the launcher's transcript gates depend on: the id
    /// `--resume` opens is exactly `claude_transcript_id`. If these ever drift,
    /// a gate checks the existence of a different file than the spawn uses —
    /// which is the bug class this whole seam exists to close.
    #[test]
    fn resume_target_is_the_shared_transcript_id() {
        for conversation in [None, Some(uuid::Uuid::from_u128(99))] {
            let portal_id = uuid::Uuid::from_u128(2);
            let args = claude_cli_args(portal_id, true, conversation, None, false, &[]);
            let expected = crate::transcript::claude_transcript_id(portal_id, conversation);
            assert_eq!(
                &args[args.len() - 2..],
                ["--resume", &expected.to_string()],
                "argv and the gates' transcript id must agree (conversation={conversation:?})"
            );
        }
    }

    /// Tools claude spawns must inherit the *portal* session id. Claude's own
    /// `CLAUDE_CODE_SESSION_ID` names its conversation, which `/clear` rolls
    /// onto an id that is not a portal session at all — leaving
    /// `agent-portal message send` to guess from host + working directory.
    #[test]
    fn exports_portal_session_id_to_the_claude_child() {
        let config = SessionConfig {
            session_id: uuid::Uuid::from_u128(42),
            working_directory: std::env::temp_dir(),
            // A cleared session: claude's own env var would name this instead.
            claude_conversation_id: Some(uuid::Uuid::from_u128(4242)),
            ..Default::default()
        };
        let cmd = build_claude_command(Path::new("claude"), &[], &config);

        let envs: Vec<(String, String)> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(
            envs.iter()
                .any(|(k, v)| k == "PORTAL_SESSION_ID" && *v == config.session_id.to_string()),
            "PORTAL_SESSION_ID must be the portal id, not the conversation; envs: {envs:?}"
        );
    }
}

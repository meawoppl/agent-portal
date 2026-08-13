//! Git branch / PR metadata detection and session update emission.

use claude_codes::io::ContentBlock;
use claude_codes::ClaudeOutput;
use muse_codes::io::ToolResult;
pub(super) use session_lib::git_metadata::{
    get_branch_info, get_open_prs, get_pr_url, GitMetadataState, GitRefreshTrigger,
};
pub use session_lib::git_metadata::{get_git_branch, get_repo_url};
use shared::ProxyToServer;
use tracing::{debug, error};
use uuid::Uuid;

use super::SharedWsWrite;

/// The CLI's own "this session now has a published PR/MR" signal
/// (`system/code_change_published`, claude-codes 2.1.163+), if this is one.
///
/// This is more immediate and authoritative than the heuristic
/// [`claude_output_has_git_signal`] scan of bash commands: it fires the moment
/// the CLI publishes, so the caller can refresh PR metadata now instead of
/// waiting for the next deferred git poll. We still route it through the shared
/// `gh`-backed refresh rather than trusting the event's URL blindly, so branch /
/// repo / open-PR fields stay consistent with everything else on the session.
pub(super) fn claude_output_code_change_published(
    output: &ClaudeOutput,
) -> Option<claude_codes::CodeChangePublishedMessage> {
    match output {
        ClaudeOutput::System(sys) => sys.as_code_change_published(),
        _ => None,
    }
}

pub(super) fn claude_output_has_git_signal(output: &ClaudeOutput) -> bool {
    if let ClaudeOutput::User(user) = output {
        for block in &user.message.content {
            if let ContentBlock::ToolResult(tr) = block {
                if tr
                    .content
                    .as_ref()
                    .is_some_and(|content| text_has_git_signal(&format!("{:?}", content)))
                {
                    return true;
                }
            }
        }
    }

    if let Some(bash) = output.as_tool_use("Bash") {
        if let Some(claude_codes::tool_inputs::ToolInput::Bash(input)) = bash.typed_input() {
            return text_has_git_signal(&input.command);
        }
    }
    false
}

pub(super) fn codex_output_has_git_signal(value: &serde_json::Value) -> bool {
    let Some(event_type) = value.get("type").and_then(|t| t.as_str()) else {
        return false;
    };
    if !matches!(
        event_type,
        "item.started" | "item.updated" | "item.completed"
    ) {
        return false;
    }

    let Some(item) = value.get("item") else {
        return false;
    };
    let Some(item_type) = item.get("type").and_then(|t| t.as_str()) else {
        return false;
    };
    if item_type != "commandExecution" && item_type != "command_execution" {
        return false;
    }

    item.get("command")
        .and_then(|command| command.as_str())
        .is_some_and(text_has_git_signal)
        || item
            .get("aggregatedOutput")
            .or_else(|| item.get("aggregated_output"))
            .and_then(|output| output.as_str())
            .is_some_and(text_has_git_signal)
}

/// Muse's counterpart to [`codex_output_has_git_signal`].
///
/// Muse cannot reuse the codex detector: its protocol is an event-sourced
/// journal, so where codex emits `item.completed` carrying a
/// `commandExecution`, muse emits `MuseWireEvent { type: "muse_record",
/// payload_type: "tool.result", payload }`. The codex predicate therefore
/// returned `false` for every muse record it would ever see, which left muse
/// sessions refreshing git metadata only on `GitRefreshTrigger`'s
/// every-100-messages fallback — never in response to an actual git operation
/// (#1653).
///
/// The payload is deserialized into muse-codes' typed [`ToolResult`] rather
/// than read field-by-field, so a schema change upstream is a compile error
/// here instead of a predicate that silently stops matching.
///
/// Note this does *not* gate on [`ToolResult::is_command_tool`]: compact
/// results omit `correlation_facts` entirely, and muse-codes documents
/// `command_result()` as parsing those too. A successful parse into
/// [`CommandResult`] is itself the evidence that this was a shell command.
pub(super) fn muse_output_has_git_signal(value: &serde_json::Value) -> bool {
    if value.get("type").and_then(|t| t.as_str()) != Some("muse_record") {
        return false;
    }
    if value.get("payload_type").and_then(|t| t.as_str()) != Some("tool.result") {
        return false;
    }
    let Some(payload) = value.get("payload") else {
        return false;
    };
    let Ok(tool_result) = serde_json::from_value::<ToolResult>(payload.clone()) else {
        return false;
    };
    let Some(command) = tool_result.command_result() else {
        return false;
    };
    text_has_git_signal(&command.command) || text_has_git_signal(&command.output)
}

fn text_has_git_signal(text: &str) -> bool {
    text.contains("git ")
        || text.contains("gh ")
        || text.contains("branch")
        || text.contains("checkout")
        || text.contains("merge")
        || text.contains("rebase")
        || text.contains("commit")
}

/// Check and send git branch, PR URL, or repo URL update if changed.
pub(super) async fn check_and_send_branch_update(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    working_directory: &str,
    state: &GitMetadataState,
) {
    let info = get_branch_info(working_directory);
    let new_branch = info.as_ref().map(|i| i.display());
    // PR lookup keys on the branch the work ships from — the active
    // worktree's when one exists (#1067), never the composite display form.
    let new_pr_url = info
        .as_ref()
        .and_then(|i| get_pr_url(working_directory, i.pr_branch()));
    let new_repo_url = get_repo_url(working_directory);
    let new_open_prs = get_open_prs(working_directory);

    let mut branch_guard = state.current_branch.lock().await;
    let mut pr_guard = state.current_pr_url.lock().await;
    let mut repo_guard = state.current_repo_url.lock().await;
    let mut open_prs_guard = state.current_open_prs.lock().await;

    let branch_changed = *branch_guard != new_branch;
    let pr_changed = *pr_guard != new_pr_url;
    let repo_changed = *repo_guard != new_repo_url;
    let open_prs_changed = *open_prs_guard != new_open_prs;

    if branch_changed || pr_changed || repo_changed || open_prs_changed {
        if branch_changed {
            debug!(
                "Git branch changed: {:?} -> {:?}",
                *branch_guard, new_branch
            );
        }
        if pr_changed {
            debug!("PR URL changed: {:?} -> {:?}", *pr_guard, new_pr_url);
        }
        if open_prs_changed {
            debug!(
                "Open PRs changed: {} -> {}",
                open_prs_guard.len(),
                new_open_prs.len()
            );
        }
        *branch_guard = new_branch.clone();
        *pr_guard = new_pr_url.clone();
        *repo_guard = new_repo_url.clone();
        *open_prs_guard = new_open_prs.clone();

        drop(branch_guard);
        drop(pr_guard);
        drop(repo_guard);
        drop(open_prs_guard);

        let update_msg = ProxyToServer::SessionUpdate {
            session_id,
            git_branch: new_branch,
            pr_url: new_pr_url,
            repo_url: new_repo_url,
            open_prs: new_open_prs,
        };

        let mut ws = ws_write.lock().await;
        if let Err(e) = ws.send(update_msg).await {
            error!("Failed to send branch update: {}", e);
        }
    }
}

/// Cheap input-path refresh: only pay for PR/repo lookup when the branch changed.
pub(super) async fn check_and_send_branch_update_if_branch_changed(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    working_directory: &str,
    state: &GitMetadataState,
) {
    let new_branch = get_git_branch(working_directory);
    let branch_changed = {
        let branch_guard = state.current_branch.lock().await;
        *branch_guard != new_branch
    };

    if branch_changed {
        check_and_send_branch_update(ws_write, session_id, working_directory, state).await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Build the `MuseWireEvent` shape the muse classifier puts on the wire
    /// for a `tool.result` record whose text is a `CommandResult`.
    fn muse_tool_result(command: &str, output: &str) -> serde_json::Value {
        let command_result = json!({
            "chunk_id": "exec-1",
            "command": command,
            "description": "do a thing",
            "exit_code": 0,
            "terminal_status": "completed",
            "output": output,
            "original_output_bytes": 0,
            "original_output_tokens": 0,
            "truncated": false,
        });
        json!({
            "type": "muse_record",
            "payload_type": "tool.result",
            "stream_id": "run-1",
            "record_id": "rec-1",
            "causation_id": "cause-1",
            "sequence": 1,
            "durability": "durable",
            "record_type": "event",
            "recorded_at": 0,
            "payload": {
                "kind": "tool.result",
                "command_id": "cmd-1",
                "run_stream": { "kind": "run", "id": "run-1" },
                "call_id": "call-1",
                "text": command_result.to_string(),
                "correlation_facts": { "tool_name": "bash" },
            }
        })
    }

    #[test]
    fn muse_git_signal_detects_a_git_command() {
        assert!(muse_output_has_git_signal(&muse_tool_result(
            "git checkout -b feature/muse-branch",
            ""
        )));
    }

    /// Parity with the codex detector, which also scans command output —
    /// `gh pr create` prints the PR URL rather than naming it in the command.
    #[test]
    fn muse_git_signal_detects_a_signal_in_command_output() {
        assert!(muse_output_has_git_signal(&muse_tool_result(
            "make ship",
            "switched to branch feature/test"
        )));
    }

    #[test]
    fn muse_git_signal_ignores_unrelated_commands() {
        assert!(!muse_output_has_git_signal(&muse_tool_result(
            "cargo test --workspace",
            "test result: ok"
        )));
    }

    /// Records that are not command results must not be scanned — a model
    /// message mentioning "commit" is not a git operation.
    #[test]
    fn muse_git_signal_ignores_non_tool_result_records() {
        let value = json!({
            "type": "muse_record",
            "payload_type": "run.output.delta",
            "stream_id": "run-1",
            "record_id": "rec-2",
            "causation_id": "cause-1",
            "sequence": 2,
            "durability": "ephemeral",
            "record_type": "event",
            "recorded_at": 0,
            "payload": { "text": "next I will commit this on a new branch" }
        });
        assert!(!muse_output_has_git_signal(&value));
    }

    /// The regression that motivated #1653: the codex predicate is structurally
    /// incapable of matching a muse record, so muse could never mark a signal.
    #[test]
    fn codex_detector_never_matches_muse_records() {
        let muse = muse_tool_result("git checkout -b feature/muse-branch", "");
        assert!(!codex_output_has_git_signal(&muse));
        assert!(muse_output_has_git_signal(&muse));
    }

    #[test]
    fn codex_git_signal_detects_command_events() {
        let value = json!({
            "type": "item.completed",
            "item": {
                "type": "commandExecution",
                "id": "cmd-1",
                "command": "git checkout -b feature/codex-branch",
                "aggregatedOutput": "",
                "status": "completed"
            }
        });

        assert!(codex_output_has_git_signal(&value));
    }

    #[test]
    fn codex_git_signal_detects_snake_case_command_events() {
        let value = json!({
            "type": "item.updated",
            "item": {
                "type": "command_execution",
                "id": "cmd-1",
                "command": "printf done",
                "aggregated_output": "switched to branch feature/test",
                "status": "completed"
            }
        });

        assert!(codex_output_has_git_signal(&value));
    }

    #[test]
    fn codex_git_signal_ignores_non_command_events() {
        let value = json!({
            "type": "item.completed",
            "item": {
                "type": "agentMessage",
                "id": "msg-1",
                "text": "run git status next"
            }
        });

        assert!(!codex_output_has_git_signal(&value));
    }

    #[test]
    fn git_refresh_trigger_defers_after_git_signal() {
        let mut trigger = GitRefreshTrigger::default();

        assert!(!trigger.should_check_before_message());
        trigger.mark_git_signal();
        assert!(trigger.should_check_before_message());
        assert!(!trigger.should_check_before_message());
    }

    #[test]
    fn git_refresh_trigger_checks_every_hundred_messages() {
        let mut trigger = GitRefreshTrigger::default();

        for _ in 0..99 {
            assert!(!trigger.should_check_before_message());
        }
        assert!(trigger.should_check_before_message());
    }

    #[test]
    fn code_change_published_is_recognized_and_carries_the_url() {
        let output: ClaudeOutput = serde_json::from_value(json!({
            "type": "system",
            "subtype": "code_change_published",
            "provider": "github",
            "url": "https://github.com/meawoppl/agent-portal/pull/1473",
            "repo": "meawoppl/agent-portal",
            "identifier": "1473",
            "uuid": "u-1",
            "session_id": "s-1",
        }))
        .expect("valid code_change_published frame");

        let published =
            claude_output_code_change_published(&output).expect("should detect the signal");
        assert_eq!(
            published.url,
            "https://github.com/meawoppl/agent-portal/pull/1473"
        );
        assert_eq!(published.provider, "github");
    }

    #[test]
    fn other_system_messages_are_not_code_change_published() {
        let output: ClaudeOutput = serde_json::from_value(json!({
            "type": "system",
            "subtype": "init",
            "session_id": "s-1",
        }))
        .expect("valid system frame");
        assert!(claude_output_code_change_published(&output).is_none());
    }
}

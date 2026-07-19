//! Git branch / PR metadata detection and session update emission.

use claude_codes::io::ContentBlock;
use claude_codes::ClaudeOutput;
pub(super) use session_lib::git_metadata::{
    get_branch_info, get_open_prs, get_pr_url, GitMetadataState, GitRefreshTrigger,
};
pub use session_lib::git_metadata::{get_git_branch, get_repo_url};
use shared::ProxyToServer;
use tracing::{debug, error};
use uuid::Uuid;

use super::SharedWsWrite;

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
}

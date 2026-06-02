//! Git branch / PR metadata detection and session update emission.

use std::sync::Arc;

use claude_codes::io::ContentBlock;
use claude_codes::ClaudeOutput;
use shared::ProxyToServer;
use tokio::sync::Mutex;
use tracing::{debug, error};
use uuid::Uuid;

use super::SharedWsWrite;

#[derive(Clone)]
pub(super) struct GitMetadataState {
    pub current_branch: Arc<Mutex<Option<String>>>,
    pub current_pr_url: Arc<Mutex<Option<String>>>,
    pub current_repo_url: Arc<Mutex<Option<String>>>,
}

impl GitMetadataState {
    pub fn new(git_branch: Option<String>) -> Self {
        Self {
            current_branch: Arc::new(Mutex::new(git_branch)),
            current_pr_url: Arc::new(Mutex::new(None)),
            current_repo_url: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Default)]
pub(super) struct GitRefreshTrigger {
    message_count: u64,
    pending_git_check: bool,
}

impl GitRefreshTrigger {
    pub fn should_check_before_message(&mut self) -> bool {
        self.message_count += 1;
        let should_check = self.pending_git_check || self.message_count.is_multiple_of(100);
        self.pending_git_check = false;
        should_check
    }

    pub fn mark_git_signal(&mut self) {
        self.pending_git_check = true;
    }
}

/// Get the current git branch name, if in a git repository.
pub(super) fn get_git_branch(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    if branch == "HEAD" {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| format!("detached:{}", s.trim()))
    } else {
        Some(branch)
    }
}

/// Look up the GitHub repository URL using the `gh` CLI.
pub(super) fn get_repo_url(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("gh")
        .args(["repo", "view", "--json", "url", "-q", ".url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Look up the GitHub PR URL for a branch using the `gh` CLI.
pub(super) fn get_pr_url(cwd: &str, branch: &str) -> Option<String> {
    if branch == "main" || branch == "master" || branch.starts_with("detached:") {
        return None;
    }
    let output = std::process::Command::new("gh")
        .args(["pr", "view", branch, "--json", "url", "-q", ".url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
    let new_branch = get_git_branch(working_directory);
    let new_pr_url = new_branch
        .as_deref()
        .and_then(|b| get_pr_url(working_directory, b));
    let new_repo_url = get_repo_url(working_directory);

    let mut branch_guard = state.current_branch.lock().await;
    let mut pr_guard = state.current_pr_url.lock().await;
    let mut repo_guard = state.current_repo_url.lock().await;

    let branch_changed = *branch_guard != new_branch;
    let pr_changed = *pr_guard != new_pr_url;
    let repo_changed = *repo_guard != new_repo_url;

    if branch_changed || pr_changed || repo_changed {
        if branch_changed {
            debug!(
                "Git branch changed: {:?} -> {:?}",
                *branch_guard, new_branch
            );
        }
        if pr_changed {
            debug!("PR URL changed: {:?} -> {:?}", *pr_guard, new_pr_url);
        }
        *branch_guard = new_branch.clone();
        *pr_guard = new_pr_url.clone();
        *repo_guard = new_repo_url.clone();

        drop(branch_guard);
        drop(pr_guard);
        drop(repo_guard);

        let update_msg = ProxyToServer::SessionUpdate {
            session_id,
            git_branch: new_branch,
            pr_url: new_pr_url,
            repo_url: new_repo_url,
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

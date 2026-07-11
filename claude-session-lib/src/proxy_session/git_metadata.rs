//! Git branch / PR metadata detection and session update emission.

use std::sync::Arc;

use claude_codes::io::ContentBlock;
use claude_codes::ClaudeOutput;
use shared::{PrRef, ProxyToServer};
use tokio::sync::Mutex;
use tracing::{debug, error};
use uuid::Uuid;

use super::SharedWsWrite;

#[derive(Clone)]
pub(super) struct GitMetadataState {
    pub current_branch: Arc<Mutex<Option<String>>>,
    pub current_pr_url: Arc<Mutex<Option<String>>>,
    pub current_repo_url: Arc<Mutex<Option<String>>>,
    pub current_open_prs: Arc<Mutex<Vec<PrRef>>>,
}

impl GitMetadataState {
    pub fn new(git_branch: Option<String>) -> Self {
        Self {
            current_branch: Arc::new(Mutex::new(git_branch)),
            current_pr_url: Arc::new(Mutex::new(None)),
            current_repo_url: Arc::new(Mutex::new(None)),
            current_open_prs: Arc::new(Mutex::new(Vec::new())),
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

/// Branch detection result, worktree-aware (#1067).
pub struct GitBranchInfo {
    /// Branch checked out in the session's own working directory.
    pub checkout: String,
    /// Branch of the most-recently-active *other* worktree of the same
    /// repo, when its HEAD moved more recently than the session
    /// checkout's. Agents routinely do their real work in `git worktree`
    /// checkouts the session cwd knows nothing about.
    pub active_worktree: Option<String>,
}

impl GitBranchInfo {
    /// The pill string: `checkout (+ active)` when an outside worktree is
    /// where the action is, plain `checkout` otherwise.
    pub fn display(&self) -> String {
        match &self.active_worktree {
            Some(active) => format!("{} (+ {})", self.checkout, active),
            None => self.checkout.clone(),
        }
    }

    /// The branch to use for PR lookups: PRs ship from the branch being
    /// worked on, which is the active worktree's when one exists.
    pub fn pr_branch(&self) -> &str {
        self.active_worktree.as_deref().unwrap_or(&self.checkout)
    }
}

/// Get the current git branch name, if in a git repository. Worktree-aware:
/// see [`GitBranchInfo::display`] for the composite form.
pub fn get_git_branch(cwd: &str) -> Option<String> {
    get_branch_info(cwd).map(|info| info.display())
}

/// Worktree-aware branch detection (#1067).
pub(super) fn get_branch_info(cwd: &str) -> Option<GitBranchInfo> {
    let checkout = checkout_branch(cwd)?;
    let active_worktree = most_recently_active_worktree_branch(cwd)
        .filter(|(branch, _)| *branch != checkout)
        .filter(|(_, mtime)| {
            // Only surface the outside worktree while it's genuinely the
            // more recent site of activity.
            head_mtime_for_checkout(cwd).is_none_or(|cwd_mtime| *mtime > cwd_mtime)
        })
        .map(|(branch, _)| branch);
    Some(GitBranchInfo {
        checkout,
        active_worktree,
    })
}

/// The branch checked out at `cwd` (the pre-#1067 one-shot behavior).
fn checkout_branch(cwd: &str) -> Option<String> {
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

/// When did this checkout last see git activity? The per-worktree reflog
/// (`logs/HEAD`) is appended by every commit/checkout/reset made in that
/// worktree, so its mtime is a cheap, robust activity signal. (`HEAD`
/// itself only changes on branch switches.) Falls back to `HEAD` for
/// reflog-disabled repos. For a linked worktree `<path>/.git` is a file
/// containing `gitdir: <dir>`; for the main checkout it's the `.git`
/// directory itself.
fn head_mtime(worktree_path: &std::path::Path) -> Option<std::time::SystemTime> {
    let dot_git = worktree_path.join(".git");
    let git_dir = if dot_git.is_file() {
        let contents = std::fs::read_to_string(&dot_git).ok()?;
        std::path::PathBuf::from(contents.strip_prefix("gitdir:")?.trim())
    } else {
        dot_git
    };
    std::fs::metadata(git_dir.join("logs/HEAD"))
        .or_else(|_| std::fs::metadata(git_dir.join("HEAD")))
        .ok()?
        .modified()
        .ok()
}

fn head_mtime_for_checkout(cwd: &str) -> Option<std::time::SystemTime> {
    // The session cwd may be a subdirectory; resolve the worktree root.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let top = String::from_utf8(output.stdout).ok()?;
    head_mtime(std::path::Path::new(top.trim()))
}

/// Enumerate this repo's worktrees and return the branch + HEAD mtime of the
/// most recently active one (excluding detached checkouts, which have no
/// branch to display).
fn most_recently_active_worktree_branch(cwd: &str) -> Option<(String, std::time::SystemTime)> {
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;

    let mut best: Option<(String, std::time::SystemTime)> = None;
    let mut current_path: Option<std::path::PathBuf> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(std::path::PathBuf::from(path));
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            let branch = branch_ref
                .strip_prefix("refs/heads/")
                .unwrap_or(branch_ref)
                .to_string();
            if let Some(mtime) = current_path.as_deref().and_then(head_mtime) {
                if best.as_ref().is_none_or(|(_, t)| mtime > *t) {
                    best = Some((branch, mtime));
                }
            }
        }
    }
    best
}

/// Look up the GitHub repository URL using the `gh` CLI.
pub fn get_repo_url(cwd: &str) -> Option<String> {
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

/// List all open PRs in the repo via the `gh` CLI, sorted by number ascending.
/// Returns an empty list if `gh` is unavailable, errors, or there are none.
pub(super) fn get_open_prs(cwd: &str) -> Vec<PrRef> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,url,headRefName",
        ])
        .current_dir(cwd)
        .output()
        .ok();
    let Some(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    // Parse via Value so we don't depend on a serde-derive struct here.
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or(serde_json::Value::Null);
    let mut prs: Vec<PrRef> = parsed
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let number = item.get("number")?.as_i64()?;
                    let url = item.get("url")?.as_str()?.to_string();
                    let branch = item
                        .get("headRefName")
                        .and_then(|b| b.as_str())
                        .unwrap_or_default()
                        .to_string();
                    Some(PrRef {
                        number,
                        url,
                        branch,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    prs.sort_by_key(|p| p.number);
    prs
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

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// #1067: a sibling worktree with more recent HEAD activity surfaces as
    /// the active branch — in the display composite AND as the PR-lookup
    /// branch — and drops back out when the main checkout is active again.
    #[test]
    fn branch_info_surfaces_most_recently_active_worktree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let cwd = repo.to_str().unwrap();

        // Single worktree: plain branch, no composite.
        let info = get_branch_info(cwd).expect("in a repo");
        assert_eq!(info.checkout, "main");
        assert_eq!(info.active_worktree, None);
        assert_eq!(info.display(), "main");
        assert_eq!(info.pr_branch(), "main");

        // Add a worktree; its HEAD was just written, so it's the most
        // recently active checkout.
        let side = tmp.path().join("side");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                side.to_str().unwrap(),
                "-b",
                "feature-side",
            ],
        );
        let info = get_branch_info(cwd).expect("in a repo");
        assert_eq!(info.checkout, "main");
        assert_eq!(info.active_worktree.as_deref(), Some("feature-side"));
        assert_eq!(info.display(), "main (+ feature-side)");
        assert_eq!(info.pr_branch(), "feature-side");

        // Activity moves back to the main checkout (HEAD rewritten):
        // the composite drops away.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "more"]);
        let info = get_branch_info(cwd).expect("in a repo");
        assert_eq!(info.active_worktree, None, "cwd is the active checkout");
        assert_eq!(info.display(), "main");
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
}

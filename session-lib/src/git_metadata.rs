//! Agent-agnostic git branch / repository / PR metadata discovery.

use std::sync::Arc;

use shared::PrRef;
use tokio::sync::Mutex;

/// Shared git metadata state tracked by long-lived session loops.
#[derive(Clone)]
pub struct GitMetadataState {
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
pub struct GitRefreshTrigger {
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
pub fn get_branch_info(cwd: &str) -> Option<GitBranchInfo> {
    let checkout = checkout_branch(cwd)?;
    let active_worktree = most_recently_active_worktree_branch(cwd)
        .filter(|(branch, _)| *branch != checkout)
        .filter(|(_, mtime)| {
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
pub fn get_pr_url(cwd: &str, branch: &str) -> Option<String> {
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
pub fn get_open_prs(cwd: &str) -> Vec<PrRef> {
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
    // Parse via Value so callers do not depend on a serde-derive struct here.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_fixtures::GitFixture;
    use std::time::Duration;

    /// #1067: a sibling worktree with more recent HEAD activity surfaces as
    /// the active branch — in the display composite AND as the PR-lookup
    /// branch — and drops back out when the main checkout is active again.
    ///
    /// Activity ordering is set by backdating reflog mtimes rather than
    /// sleeping past filesystem timestamp granularity: the ranking is exactly
    /// what the assertions describe, and the test costs no wall-clock time.
    #[test]
    fn branch_info_surfaces_most_recently_active_worktree() {
        let fixture = GitFixture::new();
        let root = fixture.root();
        let cwd = root.to_str().expect("utf-8 tempdir path");

        // Single worktree: plain branch, no composite.
        let info = get_branch_info(cwd).expect("in a repo");
        assert_eq!(info.checkout, "main");
        assert_eq!(info.active_worktree, None);
        assert_eq!(info.display(), "main");
        assert_eq!(info.pr_branch(), "main");

        // Add a worktree, then age the main checkout so the worktree is
        // unambiguously the more recently active one.
        let side = fixture.add_worktree("side", "feature-side");
        fixture.age_head_log(&root, Duration::from_secs(60));

        let info = get_branch_info(cwd).expect("in a repo");
        assert_eq!(info.checkout, "main");
        assert_eq!(info.active_worktree.as_deref(), Some("feature-side"));
        assert_eq!(info.display(), "main (+ feature-side)");
        assert_eq!(info.pr_branch(), "feature-side");

        // Activity moves back to the main checkout: the composite drops away.
        fixture.age_head_log(&side, Duration::from_secs(120));
        let info = get_branch_info(cwd).expect("in a repo");
        assert_eq!(info.active_worktree, None, "cwd is the active checkout");
        assert_eq!(info.display(), "main");
    }

    /// A detached HEAD has no branch name to show, so the checkout reads as
    /// `detached:<short sha>` rather than git's literal `HEAD`.
    #[test]
    fn detached_head_reports_a_short_sha() {
        let fixture = GitFixture::new();
        fixture.detach_head();
        let root = fixture.root();
        let info = get_branch_info(root.to_str().expect("utf-8 path")).expect("in a repo");

        assert!(
            info.checkout.starts_with("detached:"),
            "expected a detached marker, got {}",
            info.checkout
        );
        assert_ne!(info.checkout, "detached:", "the short sha must be present");
    }

    /// Outside a repository there is no branch info at all — the callers use
    /// `None` to mean "not a git checkout", not "unknown branch".
    #[test]
    fn non_repository_has_no_branch_info() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(get_branch_info(tmp.path().to_str().expect("utf-8 path")).is_none());
    }
}

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
}

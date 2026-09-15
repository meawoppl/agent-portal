//! Opt-in git worktree creation for launched sessions (issue #1325).
//!
//! When a launch request sets `create_worktree`, the launcher creates a git
//! worktree from the repository containing the requested directory and runs the
//! session inside it, instead of running directly in the requested directory.
//!
//! ## Design decisions
//!
//! - **Location.** Worktrees live under `<repo_root>/.worktrees/<branch>`, where
//!   `<repo_root>` is the top level reported by `git rev-parse --show-toplevel`.
//!   Keeping them inside the repo guarantees they stay under the launcher user's
//!   home directory (the launcher refuses to run anywhere else) and keeps them
//!   discoverable next to the repo they belong to. Add `/.worktrees/` to the
//!   repo's `.gitignore` so the checkouts don't show up as untracked files in
//!   the primary working tree.
//! - **Branch naming.** A caller-supplied branch is sanitized to a git-safe
//!   name. When none is given we derive `session-<YYYYMMDD-HHMMSS>` so each new
//!   session gets its own branch.
//! - **Idempotency.** If the target worktree path already exists we reuse it
//!   (a resume/relaunch shouldn't fail because the worktree is already there).
//!   If the branch already exists we check it out into the new worktree instead
//!   of trying to create it again.
//! - **Lifecycle / cleanup.** v1 leaves cleanup manual: worktrees are *not*
//!   removed when the session ends. Remove them with
//!   `git worktree remove <path>` (and optionally delete the branch) when you no
//!   longer need them. Automatic cleanup on session end is intentionally out of
//!   scope for v1 — a session may end while its work is still uncommitted.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

/// Provenance needed to prove a launcher-owned scratch checkout is disposable.
pub struct ScratchWorktree {
    pub path: PathBuf,
    repo_root: PathBuf,
    branch: String,
    base_commit: String,
}

/// Create (or reuse) a git worktree for a session.
///
/// `base_dir` must be an existing directory inside a git repository. Returns the
/// path to the worktree the session should run in.
pub fn create_worktree(base_dir: &Path, branch: Option<&str>) -> Result<PathBuf> {
    let repo_root = git_repo_root(base_dir).with_context(|| {
        format!(
            "Cannot create a worktree: {} is not inside a git repository",
            base_dir.display()
        )
    })?;

    let branch_name = branch
        .map(sanitize_branch_name)
        .filter(|b| !b.is_empty())
        .unwrap_or_else(default_branch_name);

    let worktree_path = repo_root.join(".worktrees").join(&branch_name);

    // Reuse an existing worktree checkout rather than failing on relaunch.
    if worktree_path.is_dir() {
        info!(
            "Reusing existing git worktree at {}",
            worktree_path.display()
        );
        return worktree_path.canonicalize().with_context(|| {
            format!(
                "Failed to resolve worktree path {}",
                worktree_path.display()
            )
        });
    }

    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create worktree parent directory {}",
                parent.display()
            )
        })?;
    }

    let worktree_str = worktree_path.to_string_lossy().to_string();

    // Try to create a brand-new branch for this worktree.
    let create_new = run_git(
        &repo_root,
        &["worktree", "add", "-b", &branch_name, &worktree_str],
    )?;

    if !create_new.success {
        // The branch may already exist — check it out into the worktree instead.
        let check_out_existing = run_git(
            &repo_root,
            &["worktree", "add", &worktree_str, &branch_name],
        )?;
        if !check_out_existing.success {
            anyhow::bail!(
                "git worktree add failed: {} (retry with existing branch: {})",
                create_new.stderr.trim(),
                check_out_existing.stderr.trim()
            );
        }
    }

    info!(
        "Created git worktree at {} on branch {}",
        worktree_path.display(),
        branch_name
    );

    worktree_path.canonicalize().with_context(|| {
        format!(
            "Failed to resolve worktree path {}",
            worktree_path.display()
        )
    })
}

/// Create or reuse a launcher-owned scratch worktree under the user's home.
pub fn create_scratch_worktree(base_dir: &Path, branch: &str) -> Result<ScratchWorktree> {
    let repo_root = git_repo_root(base_dir).with_context(|| {
        format!(
            "Cannot create a scratch worktree: {} is not inside a git repository",
            base_dir.display()
        )
    })?;
    let branch = sanitize_branch_name(branch);
    if branch.is_empty() {
        anyhow::bail!("Scratch worktree branch is empty after sanitization");
    }
    let repo_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repository");
    let repo_key = format!(
        "{}-{:016x}",
        sanitize_branch_name(repo_name),
        stable_path_hash(&repo_root)
    );
    let path = crate::path_policy::home_dir()?
        .join(".agent-portal/worktrees")
        .join(repo_key)
        .join(&branch);
    let base_commit = git_stdout(&repo_root, &["rev-parse", "HEAD"])?;

    if !path.is_dir() {
        let parent = path
            .parent()
            .context("Scratch worktree path has no parent")?;
        std::fs::create_dir_all(parent)?;
        let path_string = path.to_string_lossy().to_string();
        let created = run_git(
            &repo_root,
            &["worktree", "add", "-b", &branch, &path_string],
        )?;
        if !created.success {
            let reused = run_git(&repo_root, &["worktree", "add", &path_string, &branch])?;
            if !reused.success {
                anyhow::bail!(
                    "git worktree add failed: {} (existing branch: {})",
                    created.stderr.trim(),
                    reused.stderr.trim()
                );
            }
        }
    }
    let path = path.canonicalize()?;
    crate::path_policy::ensure_canonical_path_under_home(&path)?;
    Ok(ScratchWorktree {
        path,
        repo_root,
        branch,
        base_commit,
    })
}

/// Remove a scratch checkout only when it has neither dirty files nor commits
/// made since it was created. Retaining uncertain work is always safer.
pub fn cleanup_scratch_worktree(scratch: ScratchWorktree) {
    if !scratch_is_disposable(&scratch.path, &scratch.base_commit) {
        warn!(path = %scratch.path.display(), "Retaining scratch worktree with changes or commits");
        return;
    }
    let path = scratch.path.to_string_lossy().to_string();
    match run_git(&scratch.repo_root, &["worktree", "remove", &path]) {
        Ok(out) if out.success => {
            let _ = run_git(&scratch.repo_root, &["branch", "-D", &scratch.branch]);
            info!(path = %scratch.path.display(), "Removed clean scratch worktree");
        }
        Ok(out) => {
            warn!(path = %scratch.path.display(), error = %out.stderr.trim(), "Failed to remove scratch worktree")
        }
        Err(error) => {
            warn!(path = %scratch.path.display(), %error, "Failed to remove scratch worktree")
        }
    }
}

fn scratch_is_disposable(path: &Path, base_commit: &str) -> bool {
    let clean = git_stdout(path, &["status", "--porcelain"])
        .map(|s| s.is_empty())
        .unwrap_or(false);
    let no_new_commits = git_stdout(
        path,
        &["rev-list", "--count", &format!("{base_commit}..HEAD")],
    )
    .map(|s| s == "0")
    .unwrap_or(false);
    clean && no_new_commits
}

/// Return the top-level directory of the git repository containing `cwd`.
fn git_repo_root(cwd: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if root.is_empty() {
        return None;
    }
    Some(PathBuf::from(root))
}

struct GitOutput {
    success: bool,
    stderr: String,
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<GitOutput> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run git {}", args.join(" ")))?;
    Ok(GitOutput {
        success: out.status.success(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

/// Stable FNV-1a key keeps repositories with the same basename separate while
/// preserving the same scratch cwd across launcher restarts.
fn stable_path_hash(path: &Path) -> u64 {
    path.to_string_lossy()
        .bytes()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
}

/// Reduce an arbitrary user string to a git-safe branch name.
///
/// Replaces any character outside `[A-Za-z0-9._/-]` with `-`, collapses runs of
/// `-`, and trims leading/trailing separators. Returns an empty string if
/// nothing usable remains (the caller then falls back to the default).
fn sanitize_branch_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '/' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    // Collapse consecutive '-' and trim separators from the ends.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_dash = false;
    for ch in out.chars() {
        if ch == '-' {
            if !prev_dash {
                collapsed.push(ch);
            }
            prev_dash = true;
        } else {
            collapsed.push(ch);
            prev_dash = false;
        }
    }
    collapsed
        .trim_matches(|c| c == '-' || c == '/' || c == '.')
        .to_string()
}

fn default_branch_name() -> String {
    // Shared shape with the backend's pre-launch display-name default
    // ([`shared::WORKTREE_BRANCH_PREFIX`]) so an unnamed worktree launch shows
    // the same name in the rail as on the branch.
    format!(
        "{}{}",
        shared::WORKTREE_BRANCH_PREFIX,
        chrono::Local::now().format(shared::WORKTREE_BRANCH_TIME_FORMAT)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_unsafe_characters() {
        assert_eq!(sanitize_branch_name("my feature"), "my-feature");
        assert_eq!(sanitize_branch_name("fix/bug #42"), "fix/bug-42");
        assert_eq!(sanitize_branch_name("  spaces  "), "spaces");
        assert_eq!(sanitize_branch_name("a~b^c:d"), "a-b-c-d");
        assert_eq!(sanitize_branch_name("--weird--"), "weird");
        assert_eq!(sanitize_branch_name("///"), "");
    }

    #[test]
    fn preserves_reasonable_branch_names() {
        assert_eq!(
            sanitize_branch_name("feature/new-thing"),
            "feature/new-thing"
        );
        assert_eq!(sanitize_branch_name("v2.1.0"), "v2.1.0");
    }

    #[test]
    fn default_branch_has_session_prefix() {
        assert!(default_branch_name().starts_with("session-"));
    }

    #[test]
    fn create_worktree_from_git_repo() {
        // Skip when git isn't available in the environment.
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }

        let tmp = std::env::temp_dir().join(format!("wt-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&tmp)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(tmp.join("README.md"), "hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let wt = create_worktree(&tmp, Some("my-session")).unwrap();
        assert!(wt.is_dir());
        assert!(wt.ends_with(".worktrees/my-session"));
        assert!(wt.join("README.md").exists());

        // Reuse path (idempotent) on a second call.
        let wt2 = create_worktree(&tmp, Some("my-session")).unwrap();
        assert_eq!(wt, wt2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn create_worktree_rejects_non_repo() {
        let tmp = std::env::temp_dir().join(format!("wt-nonrepo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let err = create_worktree(&tmp, None).unwrap_err();
        assert!(err.to_string().contains("not inside a git repository"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn scratch_cleanup_requires_clean_tree_without_new_commits() {
        let tmp = std::env::temp_dir().join(format!("scratch-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&tmp)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(tmp.join("README.md"), "base").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "base"]);
        let base = git_stdout(&tmp, &["rev-parse", "HEAD"]).unwrap();
        assert!(scratch_is_disposable(&tmp, &base));

        std::fs::write(tmp.join("README.md"), "dirty").unwrap();
        assert!(!scratch_is_disposable(&tmp, &base));
        run(&["add", "."]);
        run(&["commit", "-qm", "work"]);
        assert!(!scratch_is_disposable(&tmp, &base));
        let _ = std::fs::remove_dir_all(tmp);
    }
}

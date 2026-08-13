//! Throwaway git repositories for tests (#1656).
//!
//! Everything in [`git_metadata`](crate::git_metadata) shells out to real
//! `git`, so testing it means having a real repository to point at. Pinning a
//! branch of this repo would produce tests that drift as history moves, so
//! instead each test builds the exact shape it needs in a tempdir and throws
//! it away.
//!
//! This started as a helper local to one test and is shared because #1407's
//! commit-walk harness needs the same thing.
//!
//! Two deliberate choices make this reproducible on any machine and in CI:
//!
//! - **The ambient git config is neutralized.** `GIT_CONFIG_GLOBAL` and
//!   `GIT_CONFIG_SYSTEM` are pointed at `/dev/null` and identity comes from
//!   `GIT_AUTHOR_*` / `GIT_COMMITTER_*` env vars, so a developer's global
//!   `commit.gpgsign`, `init.defaultBranch`, or hooks cannot reach in and fail
//!   the run.
//! - **mtimes are set explicitly, never slept for.** Worktree activity
//!   ordering (#1067) is decided by the mtime of each worktree's `logs/HEAD`.
//!   Sleeping past filesystem timestamp granularity would be both slow and
//!   flaky, so [`GitFixture::age_head_log`] backdates a reflog directly.
//!
//! Available to other crates via the `test-fixtures` feature; on by default
//! inside this crate's own `cfg(test)`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

/// A disposable git repository. Deleted when dropped, along with any linked
/// worktrees created under it.
pub struct GitFixture {
    dir: TempDir,
}

impl GitFixture {
    /// A repository on branch `main` with one commit.
    ///
    /// The initial commit matters: `git rev-parse --abbrev-ref HEAD` reports
    /// the branch on an unborn HEAD too, but `git worktree add` refuses to run
    /// until there is a commit to base one on.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("create tempdir for git fixture");
        std::fs::create_dir(dir.path().join("repo")).expect("create repo dir");
        let fixture = Self { dir };
        fixture.git(&fixture.root(), &["init", "-q", "-b", "main"]);
        fixture.commit("initial");
        fixture
    }

    /// Repository root — a `repo/` subdirectory of the tempdir, so linked
    /// worktrees can be created as siblings rather than nested inside the
    /// working tree they belong to.
    pub fn root(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    /// The tempdir containing [`root`](Self::root) and any linked worktrees.
    pub fn base(&self) -> &Path {
        self.dir.path()
    }

    /// Run `git` in `cwd`, panicking with git's own stderr on failure — a
    /// fixture that fails silently produces a confusing downstream assertion
    /// rather than a readable error.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// An empty commit on the current branch — enough for branch and worktree
    /// detection, which never look at trees.
    pub fn commit(&self, message: &str) {
        self.git(
            &self.root(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
                "--no-gpg-sign",
            ],
        );
    }

    /// Create and check out a new branch in the main checkout.
    pub fn checkout_new_branch(&self, name: &str) {
        self.git(&self.root(), &["checkout", "-q", "-b", name]);
    }

    /// Detach HEAD at the current commit.
    pub fn detach_head(&self) {
        self.git(&self.root(), &["checkout", "-q", "--detach"]);
    }

    /// Add a linked worktree on a new branch, as a sibling of the repo.
    /// Returns its path.
    pub fn add_worktree(&self, dir_name: &str, branch: &str) -> PathBuf {
        let path = self.base().join(dir_name);
        let path_str = path.to_string_lossy().to_string();
        self.git(
            &self.root(),
            &["worktree", "add", "-q", "-b", branch, &path_str],
        );
        path
    }

    /// Backdate a checkout's reflog so another worktree reads as more recently
    /// active.
    ///
    /// Worktree activity ranking keys on the mtime of `logs/HEAD` within each
    /// worktree's git dir — a plain directory for the main checkout, and a
    /// `gitdir:` pointer file for a linked one. Setting the timestamp directly
    /// keeps the ordering exact instead of depending on filesystem timestamp
    /// resolution.
    pub fn age_head_log(&self, worktree_root: &Path, by: Duration) {
        let dot_git = worktree_root.join(".git");
        let git_dir = if dot_git.is_file() {
            let contents = std::fs::read_to_string(&dot_git).expect("read gitdir pointer");
            PathBuf::from(
                contents
                    .strip_prefix("gitdir:")
                    .expect("gitdir pointer is well-formed")
                    .trim(),
            )
        } else {
            dot_git
        };

        // Mirror `head_mtime`'s own fallback: the per-worktree reflog when it
        // exists, `HEAD` in reflog-disabled repos.
        let log = git_dir.join("logs/HEAD");
        let target = if log.exists() {
            log
        } else {
            git_dir.join("HEAD")
        };

        let file = std::fs::File::options()
            .write(true)
            .open(&target)
            .unwrap_or_else(|e| panic!("open {} for mtime update: {e}", target.display()));
        let when = SystemTime::now() - by;
        file.set_times(
            std::fs::FileTimes::new()
                .set_accessed(when)
                .set_modified(when),
        )
        .expect("set reflog mtime");
    }
}

impl Default for GitFixture {
    fn default() -> Self {
        Self::new()
    }
}

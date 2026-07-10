//! Derives the portal version at build time so PRs never edit a version line
//! (issue #1096): `major.minor` come from the workspace `[workspace.package]
//! version`, the patch is the git commit count.
//!
//! The count is monotonic on `main` — with squash-merge it increases by
//! exactly one per merged PR — so deployed versions stay tidy and sequential
//! while parallel PRs can't collide on a hand-picked number. Feature branches
//! naturally show a higher count (their own unmerged commits), i.e. a dev
//! build reads "ahead" of what `main` will be after squash, which is useful
//! signal. Falls back to the Cargo.toml patch when git is unavailable (a
//! source tarball or vendored build with no history).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    let major = std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap_or_else(|_| "0".into());
    let minor = std::env::var("CARGO_PKG_VERSION_MINOR").unwrap_or_else(|_| "0".into());
    let fallback_patch = std::env::var("CARGO_PKG_VERSION_PATCH").unwrap_or_else(|_| "0".into());

    // In a shallow checkout (`actions/checkout` default fetch-depth) the count
    // is truncated — usually 1 — which would ship a wrong low version. The CI
    // workflows set `fetch-depth: 0`, but guard here too so a forgotten
    // setting degrades to the Cargo.toml placeholder, never a bogus `2.13.1`.
    let shallow = git(&["rev-parse", "--is-shallow-repository"]).as_deref() == Some("true");
    let patch = if shallow {
        fallback_patch
    } else {
        git(&["rev-list", "--count", "HEAD"]).unwrap_or(fallback_patch)
    };
    println!("cargo:rustc-env=PORTAL_VERSION={major}.{minor}.{patch}");

    // Recompute only when the commit count could have changed: HEAD moving
    // (branch switch / detached commit) or the reflog appending (any
    // commit/reset/checkout). Without git, fall through to Cargo's default.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/logs/HEAD");
    }
}

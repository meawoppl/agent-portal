//! Visual-PR work delegated to this launcher host by the backend: list a
//! repo's open PRs, render a before/after summary SVG for one, or approve it —
//! all through this host's authenticated `gh`.
//!
//! Generation is self-contained and leaves nothing behind: shallow-clone the
//! repo into a `tempfile` dir, run a headless `claude` against the PR's real
//! diff there, read the SVG out, and let the `TempDir` drop reclaim the disk.
//! The backend stores the returned SVG durably; this host keeps nothing.

use shared::api::VisualPrRow;
use std::path::Path;
use std::time::Duration;
use tracing::{info, warn};

/// Ceiling on one headless generation run (clone + claude). A typical run is
/// 1–3 minutes; past ten something is wedged.
const GENERATION_TIMEOUT: Duration = Duration::from_secs(600);

/// Ceiling on plain `gh` calls (list, merge, clone).
const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// `owner/name`, nothing else — the only shape ever passed to `gh --repo` or
/// the clone. Defense in depth (the backend validates too): these strings
/// reach `Command` args directly, never a shell, but a bounded charset keeps
/// them from meaning anything surprising to `gh` itself.
pub fn repo_is_valid(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            // A leading '-' would read as a flag to gh/git.
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    ok(owner) && ok(name)
}

/// `gh pr list` for `repo`, parsed into wire rows.
pub async fn list_prs(repo: &str) -> Result<Vec<VisualPrRow>, String> {
    if !repo_is_valid(repo) {
        return Err(format!("invalid repo `{repo}` (expected owner/name)"));
    }
    let output = run_with_timeout(
        tokio::process::Command::new("gh").args([
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--limit",
            "50",
            "--json",
            "number,title,headRefName,updatedAt,isDraft,url,author",
        ]),
        GH_TIMEOUT,
    )
    .await?;

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GhPr {
        number: i64,
        title: String,
        head_ref_name: String,
        updated_at: String,
        is_draft: bool,
        url: String,
        author: GhAuthor,
    }
    #[derive(serde::Deserialize)]
    struct GhAuthor {
        login: String,
    }

    let rows: Vec<GhPr> = serde_json::from_slice(&output)
        .map_err(|e| format!("unparseable gh pr list output: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|pr| VisualPrRow {
            number: pr.number,
            title: pr.title,
            head_ref: pr.head_ref_name,
            author: pr.author.login,
            updated_at: pr.updated_at,
            draft: pr.is_draft,
            url: pr.url,
        })
        .collect())
}

/// Render the visual summary SVG for `repo`#`pr_number` in a throwaway
/// shallow clone. The `TempDir` guard reclaims the clone on every exit path.
pub async fn generate(repo: &str, pr_number: i64, model: Option<&str>) -> Result<String, String> {
    if !repo_is_valid(repo) {
        return Err(format!("invalid repo `{repo}` (expected owner/name)"));
    }
    let dir = tempfile::Builder::new()
        .prefix("visual-pr-")
        .tempdir()
        .map_err(|e| format!("could not create temp dir: {e}"))?;
    let checkout = dir.path().join("repo");

    info!("visual-pr: shallow-cloning {repo} for PR #{pr_number}");
    run_with_timeout(
        tokio::process::Command::new("gh").args([
            "repo",
            "clone",
            repo,
            &checkout.to_string_lossy(),
            "--",
            "--depth",
            "1",
        ]),
        GH_TIMEOUT,
    )
    .await
    .map_err(|e| format!("shallow clone failed: {e}"))?;

    let out_path = dir.path().join(format!("visual-pr-{pr_number}.svg"));
    let svg = run_claude(repo, pr_number, model, &checkout, &out_path).await;
    // TempDir drop cleans the clone up on success and failure alike; make the
    // intent explicit rather than relying on scope.
    drop(dir);
    svg
}

async fn run_claude(
    repo: &str,
    pr_number: i64,
    model: Option<&str>,
    checkout: &Path,
    out_path: &Path,
) -> Result<String, String> {
    let prompt = format!(
        "Render a visual before/after summary SVG for PR #{pr_number} of {repo}. \
         If this repository contains .claude/skills/visual-pr/SKILL.md, follow it exactly \
         (including its validator). Otherwise follow this compact spec: one SVG, \
         viewBox 0 0 2000 1200, full-bleed background #16161e, sans-serif; header \
         'PR #{pr_number} · <title>' in #565f89 at 24px; a 1-2 line thesis in #e6e9f5 at 34px \
         stating the claim of the change; a BEFORE panel (label #f7768e) on the left and an \
         AFTER panel (label #9ece6a) on the right split by a dashed divider at x=1000; \
         thin-bordered boxes (#3d4666 borders, #1e202e fill, #a9b1d6 body text, #7aa2f7 for \
         code identifiers) with arrows; red #f7768e only for defects, green #9ece6a only for \
         fixes, orange #e0af68 for caveats; one muted footer line naming a real limitation. \
         Ground every identifier in the actual diff: read it with `gh pr view {pr_number}` and \
         `gh pr diff {pr_number}` before drawing anything, and never invent names. \
         Write the final SVG to {out} and nothing else there. Do NOT run agent-portal show, \
         do NOT check out branches, commit, push, or modify the repository.",
        out = out_path.display(),
    );

    let mut cmd = tokio::process::Command::new("claude");
    cmd.args([
        "-p",
        &prompt,
        "--output-format",
        "json",
        "--dangerously-skip-permissions",
    ]);
    if let Some(model) = model {
        cmd.args(["--model", model]);
    }
    cmd.current_dir(checkout).stdin(std::process::Stdio::null());

    let stdout = run_with_timeout(&mut cmd, GENERATION_TIMEOUT).await?;
    // `--output-format json` ends with a ResultMessage; is_error carries the
    // run's own verdict when the process still exited 0.
    if let Ok(claude_codes::ClaudeOutput::Result(result)) =
        serde_json::from_slice::<claude_codes::ClaudeOutput>(stdout.trim_ascii())
    {
        if result.is_error {
            return Err(format!(
                "claude reported an error: {}",
                result.result.unwrap_or_else(|| "(no detail)".to_string())
            ));
        }
    }

    let svg = tokio::fs::read_to_string(out_path)
        .await
        .map_err(|e| format!("claude finished but wrote no SVG: {e}"))?;
    if !svg.trim_start().starts_with("<svg") {
        return Err("output file does not start with <svg".to_string());
    }
    Ok(svg)
}

/// `gh pr merge --squash --auto` for `repo`#`pr_number`.
pub async fn approve(repo: &str, pr_number: i64) -> Result<String, String> {
    if !repo_is_valid(repo) {
        return Err(format!("invalid repo `{repo}` (expected owner/name)"));
    }
    run_with_timeout(
        tokio::process::Command::new("gh").args([
            "pr",
            "merge",
            &pr_number.to_string(),
            "--repo",
            repo,
            "--squash",
            "--delete-branch",
            "--auto",
        ]),
        GH_TIMEOUT,
    )
    .await?;
    Ok(format!(
        "PR #{pr_number} approved — squash merge queued (auto-merge)"
    ))
}

/// Run a command with a deadline; success returns stdout, failure returns the
/// stderr tail (or the timeout/spawn reason).
async fn run_with_timeout(
    cmd: &mut tokio::process::Command,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("timed out after {}s", timeout.as_secs()))?
        .map_err(|e| format!("failed to spawn: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.trim().chars().take(400).collect();
        warn!("visual-pr command failed ({}): {tail}", output.status);
        Err(format!("exited with {}: {tail}", output.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_validation_is_strict() {
        assert!(repo_is_valid("meawoppl/agent-portal"));
        assert!(repo_is_valid("a-b/c_d.e"));
        assert!(!repo_is_valid("meawoppl"));
        assert!(!repo_is_valid("a/b/c"));
        assert!(!repo_is_valid("a b/c"));
        assert!(!repo_is_valid("-owner/--flag; rm"));
        assert!(!repo_is_valid(""));
        assert!(!repo_is_valid("owner/"));
    }
}

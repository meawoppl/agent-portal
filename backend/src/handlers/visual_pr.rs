//! Admin ▸ Visual PRs: list open pull requests and generate before/after
//! summary SVGs in the `.claude/skills/visual-pr` house style.
//!
//! Generation shells out to a headless `claude` run (driven through the
//! `claude-codes` protocol types) inside a configured git checkout
//! (`PORTAL_VISUAL_PR_REPO_DIR`); PR listing and approval shell out to `gh`
//! in the same checkout, reusing its ambient auth. This is an admin-only
//! testing feature: previews are held in memory and do not survive a backend
//! restart — regeneration is one click.

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use claude_codes::ClaudeOutput;
use serde::Deserialize;
use shared::api::{
    VisualPrApproveResponse, VisualPrItem, VisualPrListResponse, VisualPrPreviewState,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower_cookies::Cookies;
use tracing::{error, info};

use crate::{errors::AppError, handlers::admin::require_admin, AppState};

/// Ceiling on one headless generation run. A typical run is 1–3 minutes;
/// past ten something is wedged and the slot should free up.
const GENERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

// ============================================================================
// State
// ============================================================================

/// One PR's preview lifecycle. SVGs are small (≈10 KB) so they live inline.
#[derive(Debug, Clone)]
enum PreviewEntry {
    Generating,
    Ready { svg: String },
    Failed { error: String },
}

/// In-memory visual-PR runtime hung off [`AppState`]. Cloning shares the
/// entry map (AppState itself derives `Clone`).
#[derive(Clone)]
pub struct VisualPrState {
    /// Git checkout that `gh` and `claude` run in. `None` = feature disabled
    /// (the list endpoint reports why; everything else 503s).
    pub repo_dir: Option<PathBuf>,
    /// Binary to invoke for generation (default `claude`).
    pub claude_bin: String,
    entries: Arc<Mutex<HashMap<i64, PreviewEntry>>>,
}

impl VisualPrState {
    pub fn from_env() -> Self {
        let repo_dir = std::env::var("PORTAL_VISUAL_PR_REPO_DIR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from);
        let claude_bin = std::env::var("PORTAL_VISUAL_PR_CLAUDE_BIN")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "claude".to_string());
        match &repo_dir {
            Some(dir) => info!("Visual PRs: enabled, repo dir {}", dir.display()),
            None => info!("Visual PRs: disabled (PORTAL_VISUAL_PR_REPO_DIR unset)"),
        }
        Self {
            repo_dir,
            claude_bin,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn repo_dir(&self) -> Result<&PathBuf, AppError> {
        self.repo_dir.as_ref().ok_or(AppError::ServiceUnavailable(
            "visual PRs disabled: set PORTAL_VISUAL_PR_REPO_DIR",
        ))
    }

    fn entry(&self, number: i64) -> Option<PreviewEntry> {
        self.entries.lock().expect("poisoned").get(&number).cloned()
    }

    fn set_entry(&self, number: i64, entry: PreviewEntry) {
        self.entries.lock().expect("poisoned").insert(number, entry);
    }
}

// ============================================================================
// GET /api/admin/visual-prs — open PRs merged with preview state
// ============================================================================

/// Shape of one element of `gh pr list --json ...`.
#[derive(Deserialize)]
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

#[derive(Deserialize)]
struct GhAuthor {
    login: String,
}

pub async fn list_visual_prs(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<VisualPrListResponse>, AppError> {
    require_admin(&app_state, &headers, &cookies)?;

    let Some(repo_dir) = app_state.visual_prs.repo_dir.clone() else {
        return Ok(Json(VisualPrListResponse {
            enabled: false,
            disabled_reason: Some(
                "Set PORTAL_VISUAL_PR_REPO_DIR to a git checkout with gh auth to enable."
                    .to_string(),
            ),
            prs: vec![],
        }));
    };

    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "50",
            "--json",
            "number,title,headRefName,updatedAt,isDraft,url,author",
        ])
        .current_dir(&repo_dir)
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("failed to spawn gh: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("gh pr list failed: {}", stderr.trim());
        return Err(AppError::BadGatewayMessage(format!(
            "gh pr list failed: {}",
            stderr.trim()
        )));
    }

    let gh_prs: Vec<GhPr> = serde_json::from_slice(&output.stdout)
        .map_err(|e| AppError::Internal(format!("unparseable gh pr list output: {e}")))?;

    let prs = gh_prs
        .into_iter()
        .map(|pr| {
            let (preview, preview_error) = match app_state.visual_prs.entry(pr.number) {
                None => (VisualPrPreviewState::None, None),
                Some(PreviewEntry::Generating) => (VisualPrPreviewState::Generating, None),
                Some(PreviewEntry::Ready { .. }) => (VisualPrPreviewState::Ready, None),
                Some(PreviewEntry::Failed { error }) => (VisualPrPreviewState::Failed, Some(error)),
            };
            VisualPrItem {
                number: pr.number,
                title: pr.title,
                head_ref: pr.head_ref_name,
                author: pr.author.login,
                updated_at: pr.updated_at,
                draft: pr.is_draft,
                url: pr.url,
                preview,
                preview_error,
            }
        })
        .collect();

    Ok(Json(VisualPrListResponse {
        enabled: true,
        disabled_reason: None,
        prs,
    }))
}

// ============================================================================
// POST /api/admin/visual-prs/{number}/generate — kick off a background run
// ============================================================================

pub async fn generate_visual_pr(
    State(app_state): State<Arc<AppState>>,
    Path(number): Path<i64>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<StatusCode, AppError> {
    let admin = require_admin(&app_state, &headers, &cookies)?;
    app_state.visual_prs.repo_dir()?;

    if matches!(
        app_state.visual_prs.entry(number),
        Some(PreviewEntry::Generating)
    ) {
        return Err(AppError::Conflict("a generation is already running"));
    }

    info!("Visual PR #{number}: generation started by {}", admin.email);
    app_state
        .visual_prs
        .set_entry(number, PreviewEntry::Generating);
    tokio::spawn(run_generation(app_state.clone(), number));
    Ok(StatusCode::ACCEPTED)
}

/// The background generation task: headless `claude` follows the committed
/// `visual-pr` skill and writes the SVG to a temp path; we parse its final
/// `ResultMessage` (via `claude-codes`) for success, then lift the file into
/// memory.
async fn run_generation(app_state: Arc<AppState>, number: i64) {
    let result = generate_svg(&app_state, number).await;
    match result {
        Ok(svg) => {
            info!("Visual PR #{number}: preview ready ({} bytes)", svg.len());
            app_state
                .visual_prs
                .set_entry(number, PreviewEntry::Ready { svg });
        }
        Err(e) => {
            error!("Visual PR #{number}: generation failed: {e}");
            app_state
                .visual_prs
                .set_entry(number, PreviewEntry::Failed { error: e });
        }
    }
}

async fn generate_svg(app_state: &Arc<AppState>, number: i64) -> Result<String, String> {
    let repo_dir = app_state
        .visual_prs
        .repo_dir
        .clone()
        .ok_or("visual PRs disabled")?;
    let out_path = std::env::temp_dir().join(format!("visual-pr-{number}.svg"));
    // A stale file from an earlier run must not be mistaken for this run's output.
    let _ = tokio::fs::remove_file(&out_path).await;

    let prompt = format!(
        "Generate the visual PR summary for PR #{number} of this repository by following \
         .claude/skills/visual-pr/SKILL.md exactly. Read the real diff first \
         (`gh pr view {number}`, `gh pr diff {number}`) and ground every identifier in the \
         code — do not invent names. Write the final SVG to {out} and validate it with \
         `python3 .claude/skills/visual-pr/check_svg.py {out}`. Do NOT run `agent-portal show`, \
         do NOT check out branches, do NOT commit, push, or modify the repository.",
        out = out_path.display(),
    );

    let child = tokio::process::Command::new(&app_state.visual_prs.claude_bin)
        .args([
            "-p",
            &prompt,
            "--output-format",
            "json",
            "--dangerously-skip-permissions",
        ])
        .current_dir(&repo_dir)
        .stdin(std::process::Stdio::null())
        .output();

    let output = tokio::time::timeout(GENERATION_TIMEOUT, child)
        .await
        .map_err(|_| format!("timed out after {}s", GENERATION_TIMEOUT.as_secs()))?
        .map_err(|e| format!("failed to spawn claude: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .iter()
            .rev()
            .collect();
        return Err(format!(
            "claude exited with {}: {}",
            output.status,
            tail.trim()
        ));
    }

    // `--output-format json` prints a single ResultMessage object.
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<ClaudeOutput>(stdout.trim()) {
        Ok(ClaudeOutput::Result(result)) if result.is_error => {
            return Err(format!(
                "claude reported an error: {}",
                result.result.unwrap_or_else(|| "(no detail)".to_string())
            ));
        }
        Ok(_) => {}
        Err(e) => {
            // The run may still have produced the file; note the parse failure
            // only if it didn't.
            if !out_path.exists() {
                return Err(format!("unparseable claude output ({e})"));
            }
        }
    }

    let svg = tokio::fs::read_to_string(&out_path).await.map_err(|e| {
        format!(
            "claude finished but wrote no SVG at {}: {e}",
            out_path.display()
        )
    })?;
    if !svg.trim_start().starts_with("<svg") {
        return Err("output file does not start with <svg".to_string());
    }
    Ok(svg)
}

// ============================================================================
// GET /api/admin/visual-prs/{number}/preview.svg — serve a ready preview
// ============================================================================

pub async fn get_visual_pr_svg(
    State(app_state): State<Arc<AppState>>,
    Path(number): Path<i64>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app_state, &headers, &cookies)?;
    match app_state.visual_prs.entry(number) {
        Some(PreviewEntry::Ready { svg }) => Ok((
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            svg,
        )),
        _ => Err(AppError::NotFound("no preview for this PR")),
    }
}

// ============================================================================
// POST /api/admin/visual-prs/{number}/approve — squash-merge via gh
// ============================================================================

pub async fn approve_visual_pr(
    State(app_state): State<Arc<AppState>>,
    Path(number): Path<i64>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<VisualPrApproveResponse>, AppError> {
    let admin = require_admin(&app_state, &headers, &cookies)?;
    let repo_dir = app_state.visual_prs.repo_dir()?.clone();

    info!("Visual PR #{number}: approve requested by {}", admin.email);
    // `--auto` merges immediately when requirements are already met and
    // otherwise arms auto-merge for when CI goes green — either way one call.
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "merge",
            &number.to_string(),
            "--squash",
            "--delete-branch",
            "--auto",
        ])
        .current_dir(&repo_dir)
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("failed to spawn gh: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::BadGatewayMessage(format!(
            "gh pr merge failed: {}",
            stderr.trim()
        )));
    }

    Ok(Json(VisualPrApproveResponse {
        message: format!("PR #{number} approved — squash merge queued (auto-merge)"),
    }))
}

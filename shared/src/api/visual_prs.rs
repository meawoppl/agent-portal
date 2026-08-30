//! Admin ▸ Visual PRs: list open pull requests and manage generated
//! before/after summary SVGs (the `.claude/skills/visual-pr` style).

use serde::{Deserialize, Serialize};

/// Lifecycle of a PR's generated preview on the backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisualPrPreviewState {
    /// No preview has been generated for this PR (or the backend restarted;
    /// previews are held in memory only).
    None,
    /// A generation task is running.
    Generating,
    /// A preview SVG is available at `/api/admin/visual-prs/{number}/preview.svg`.
    Ready,
    /// The last generation attempt failed; see `preview_error`.
    Failed,
}

/// One open pull request as listed by the visual-PR admin panel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrItem {
    pub number: i64,
    pub title: String,
    pub head_ref: String,
    pub author: String,
    /// RFC 3339 timestamp of the PR's last update, verbatim from GitHub.
    pub updated_at: String,
    pub draft: bool,
    /// Web URL of the PR on GitHub.
    pub url: String,
    pub preview: VisualPrPreviewState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_error: Option<String>,
}

/// Response for `GET /api/admin/visual-prs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrListResponse {
    /// False when the backend has no `PORTAL_VISUAL_PR_REPO_DIR` configured;
    /// `disabled_reason` says what to set.
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    pub prs: Vec<VisualPrItem>,
}

/// Response for `POST /api/admin/visual-prs/{number}/approve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrApproveResponse {
    /// Human-readable outcome ("merge queued", trailing `gh` output, …).
    pub message: String,
}

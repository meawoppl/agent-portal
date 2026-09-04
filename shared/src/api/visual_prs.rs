//! Admin ▸ Visual PRs: list a repo's open pull requests through a chosen
//! launcher host's authenticated `gh`, generate before/after summary SVGs on
//! that host (shallow clone → headless claude → cleanup), and store the
//! results durably in the portal DB.

use serde::{Deserialize, Serialize};

/// One open PR as the launcher's `gh pr list` reports it — the wire row of
/// `LauncherToServer::VisualPrListResult`, before the backend merges preview
/// state in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrRow {
    pub number: i64,
    pub title: String,
    pub head_ref: String,
    pub author: String,
    /// RFC 3339 timestamp of the PR's last update, verbatim from GitHub.
    pub updated_at: String,
    pub draft: bool,
    /// Web URL of the PR on GitHub.
    pub url: String,
}

/// Lifecycle of a PR's generated preview on the backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisualPrPreviewState {
    /// No stored preview for this repo+PR.
    None,
    /// A generation task is running on a launcher host.
    Generating,
    /// A preview SVG is stored; fetch it at
    /// `/api/admin/visual-prs/{number}/preview.svg?repo=…`.
    Ready,
    /// The last generation attempt failed; see `preview_error`.
    Failed,
}

/// One open pull request as listed by the visual-PR admin panel: the `gh` row
/// merged with the portal's stored preview state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrItem {
    pub number: i64,
    pub title: String,
    pub head_ref: String,
    pub author: String,
    pub updated_at: String,
    pub draft: bool,
    pub url: String,
    pub preview: VisualPrPreviewState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_error: Option<String>,
}

/// Response for `GET /api/admin/visual-prs?launcher_id=…&repo=…`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrListResponse {
    pub prs: Vec<VisualPrItem>,
}

/// Body for `POST /api/admin/visual-prs/{number}/generate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrGenerateRequest {
    /// Launcher to run the generation on (must advertise the visual-PR
    /// capability and have an authenticated `gh`).
    pub launcher_id: uuid::Uuid,
    /// `owner/name`.
    pub repo: String,
    /// Model override passed to the headless claude run (`--model`); `None`
    /// uses the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Body for `POST /api/admin/visual-prs/{number}/approve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrApproveRequest {
    pub launcher_id: uuid::Uuid,
    pub repo: String,
}

/// Response for `POST /api/admin/visual-prs/{number}/approve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualPrApproveResponse {
    /// Human-readable outcome ("merge queued", trailing `gh` output, …).
    pub message: String,
}

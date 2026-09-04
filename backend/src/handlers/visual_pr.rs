//! Admin ▸ Visual PRs: list a repo's open pull requests and manage generated
//! before/after summary SVGs (the `.claude/skills/visual-pr` style).
//!
//! All `gh` and `claude` work runs on a **launcher host the admin picks in the
//! tab** — one that advertises [`shared::LAUNCHER_CAPABILITY_VISUAL_PR`] and
//! probes with an authenticated `gh`. Generation on that host is
//! self-contained (shallow clone into a tempdir, render, clean up); the
//! returned SVG is stored durably in `visual_pr_previews`, upserted per
//! `(repo, pr_number)`. Nothing is configured on the backend.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use diesel::prelude::*;
use serde::Deserialize;
use shared::api::{
    VisualPrApproveRequest, VisualPrApproveResponse, VisualPrGenerateRequest, VisualPrItem,
    VisualPrListResponse, VisualPrPreviewState,
};
use shared::{LauncherToServer, ServerToLauncher, LAUNCHER_CAPABILITY_VISUAL_PR};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tower_cookies::Cookies;
use tracing::{error, info};
use uuid::Uuid;

use crate::handlers::launchers::{launcher_rpc, require_launcher_owner};
use crate::{errors::AppError, handlers::admin::require_admin, schema, AppState};

/// Ceiling on one launcher-side generation (shallow clone + headless claude).
/// Slightly above the launcher's own 600s so its verdict arrives first.
const GENERATE_TIMEOUT_SECS: u64 = 620;
const LIST_TIMEOUT_SECS: u64 = 30;
const APPROVE_TIMEOUT_SECS: u64 = 60;

// ============================================================================
// State: in-flight/failed markers only — finished SVGs live in the DB
// ============================================================================

#[derive(Debug, Clone)]
enum Transient {
    Generating,
    Failed { error: String },
}

/// In-memory generation markers hung off [`AppState`] (cloning shares the
/// map). Ready previews are durable rows in `visual_pr_previews`.
#[derive(Clone, Default)]
pub struct VisualPrState {
    transient: Arc<Mutex<HashMap<(String, i64), Transient>>>,
}

impl VisualPrState {
    fn get(&self, repo: &str, n: i64) -> Option<Transient> {
        self.transient
            .lock()
            .expect("poisoned")
            .get(&(repo.to_string(), n))
            .cloned()
    }

    fn set(&self, repo: &str, n: i64, t: Transient) {
        self.transient
            .lock()
            .expect("poisoned")
            .insert((repo.to_string(), n), t);
    }

    fn clear(&self, repo: &str, n: i64) {
        self.transient
            .lock()
            .expect("poisoned")
            .remove(&(repo.to_string(), n));
    }
}

// ============================================================================
// Shared checks
// ============================================================================

/// `owner/name` with a bounded charset — the only repo shape relayed to a
/// launcher (which validates again before touching `gh`).
fn validate_repo(repo: &str) -> Result<(), AppError> {
    let mut parts = repo.split('/');
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) if ok(owner) && ok(name) => Ok(()),
        _ => Err(AppError::BadRequest("repo must be owner/name")),
    }
}

/// Model overrides pass straight to `claude --model`; bound the charset.
fn validate_model(model: &Option<String>) -> Result<(), AppError> {
    if let Some(m) = model {
        let ok = !m.is_empty()
            && m.len() <= 64
            && !m.starts_with('-')
            && m.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
        if !ok {
            return Err(AppError::BadRequest("invalid model name"));
        }
    }
    Ok(())
}

/// The chosen launcher must be the caller's, connected, and advertise the
/// visual-PR capability (older launchers can't decode the frames — #1366).
/// Returns the launcher's hostname for provenance stamping.
fn require_visual_pr_launcher(
    app_state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
) -> Result<String, AppError> {
    require_launcher_owner(app_state, launcher_id, user_id)?;
    let launcher = app_state
        .session_manager
        .get_launchers_for_user(&user_id)
        .into_iter()
        .find(|l| l.launcher_id == launcher_id)
        .ok_or(AppError::NotFound("Launcher not found"))?;
    if !launcher
        .capabilities
        .iter()
        .any(|c| c == LAUNCHER_CAPABILITY_VISUAL_PR)
    {
        return Err(AppError::Conflict(
            "this launcher is too old for visual PRs — update it first",
        ));
    }
    Ok(launcher.hostname)
}

// ============================================================================
// GET /api/admin/visual-prs?launcher_id=…&repo=… — open PRs + preview state
// ============================================================================

#[derive(Deserialize)]
pub struct VisualPrListQuery {
    pub launcher_id: Uuid,
    pub repo: String,
}

pub async fn list_visual_prs(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(q): Query<VisualPrListQuery>,
) -> Result<Json<VisualPrListResponse>, AppError> {
    let admin = require_admin(&app_state, &headers, &cookies)?;
    validate_repo(&q.repo)?;
    require_visual_pr_launcher(&app_state, q.launcher_id, admin.id)?;

    let request_id = Uuid::new_v4();
    let reply = launcher_rpc(
        &app_state,
        q.launcher_id,
        request_id,
        ServerToLauncher::VisualPrListPrs {
            request_id,
            repo: q.repo.clone(),
        },
        LIST_TIMEOUT_SECS,
    )
    .await?;
    let rows = match reply {
        LauncherToServer::VisualPrListResult { error: Some(e), .. } => {
            return Err(AppError::BadGatewayMessage(format!("gh pr list: {e}")))
        }
        LauncherToServer::VisualPrListResult { prs, .. } => prs,
        _ => {
            return Err(AppError::Internal(
                "unexpected launcher reply to VisualPrListPrs".into(),
            ))
        }
    };

    // Stored previews for this repo (Ready), merged under any transient state.
    let stored: Vec<i64> = {
        let mut conn = app_state.conn()?;
        schema::visual_pr_previews::table
            .filter(schema::visual_pr_previews::repo.eq(&q.repo))
            .select(schema::visual_pr_previews::pr_number)
            .load(&mut conn)?
    };
    let stored: std::collections::HashSet<i64> = stored.into_iter().collect();

    let prs = rows
        .into_iter()
        .map(|pr| {
            let (preview, preview_error) = match app_state.visual_prs.get(&q.repo, pr.number) {
                Some(Transient::Generating) => (VisualPrPreviewState::Generating, None),
                Some(Transient::Failed { error }) => (VisualPrPreviewState::Failed, Some(error)),
                None if stored.contains(&pr.number) => (VisualPrPreviewState::Ready, None),
                None => (VisualPrPreviewState::None, None),
            };
            VisualPrItem {
                number: pr.number,
                title: pr.title,
                head_ref: pr.head_ref,
                author: pr.author,
                updated_at: pr.updated_at,
                draft: pr.draft,
                url: pr.url,
                preview,
                preview_error,
            }
        })
        .collect();

    Ok(Json(VisualPrListResponse { prs }))
}

// ============================================================================
// POST /api/admin/visual-prs/{number}/generate — render on the chosen host
// ============================================================================

pub async fn generate_visual_pr(
    State(app_state): State<Arc<AppState>>,
    Path(number): Path<i64>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<VisualPrGenerateRequest>,
) -> Result<StatusCode, AppError> {
    let admin = require_admin(&app_state, &headers, &cookies)?;
    validate_repo(&req.repo)?;
    validate_model(&req.model)?;
    let hostname = require_visual_pr_launcher(&app_state, req.launcher_id, admin.id)?;

    if matches!(
        app_state.visual_prs.get(&req.repo, number),
        Some(Transient::Generating)
    ) {
        return Err(AppError::Conflict("a generation is already running"));
    }

    info!(
        "Visual PR {}#{number}: generation on {hostname} (model {:?}) by {}",
        req.repo, req.model, admin.email
    );
    app_state
        .visual_prs
        .set(&req.repo, number, Transient::Generating);
    tokio::spawn(run_generation(
        app_state.clone(),
        req,
        number,
        hostname,
        admin.id,
    ));
    Ok(StatusCode::ACCEPTED)
}

/// Background half of generate: RPC to the launcher (which clones, renders,
/// and cleans up), then persist the SVG for long-term serving.
async fn run_generation(
    app_state: Arc<AppState>,
    req: VisualPrGenerateRequest,
    number: i64,
    hostname: String,
    admin_id: Uuid,
) {
    let request_id = Uuid::new_v4();
    let reply = launcher_rpc(
        &app_state,
        req.launcher_id,
        request_id,
        ServerToLauncher::VisualPrGenerate {
            request_id,
            repo: req.repo.clone(),
            pr_number: number,
            model: req.model.clone(),
        },
        GENERATE_TIMEOUT_SECS,
    )
    .await;

    let outcome: Result<String, String> = match reply {
        Ok(LauncherToServer::VisualPrGenerateResult { svg: Some(svg), .. }) => Ok(svg),
        Ok(LauncherToServer::VisualPrGenerateResult { error, .. }) => {
            Err(error.unwrap_or_else(|| "launcher returned no SVG".into()))
        }
        Ok(_) => Err("unexpected launcher reply to VisualPrGenerate".into()),
        // launcher_rpc's failures carry static reasons; keep them readable.
        Err(AppError::GatewayTimeout(m)) | Err(AppError::BadGateway(m)) => Err(m.to_string()),
        Err(e) => Err(format!("{e:?}")),
    };

    match outcome {
        Ok(svg) => {
            let row = crate::models::NewVisualPrPreview {
                repo: req.repo.clone(),
                pr_number: number,
                svg,
                model: req.model.clone(),
                generated_on: Some(hostname),
                created_by: Some(admin_id),
            };
            let stored = app_state
                .conn()
                .map_err(|e| format!("{e:?}"))
                .and_then(|mut conn| {
                    diesel::insert_into(schema::visual_pr_previews::table)
                        .values(&row)
                        .on_conflict((
                            schema::visual_pr_previews::repo,
                            schema::visual_pr_previews::pr_number,
                        ))
                        .do_update()
                        .set((
                            schema::visual_pr_previews::svg.eq(&row.svg),
                            schema::visual_pr_previews::model.eq(&row.model),
                            schema::visual_pr_previews::generated_on.eq(&row.generated_on),
                            schema::visual_pr_previews::created_by.eq(row.created_by),
                            schema::visual_pr_previews::created_at.eq(diesel::dsl::now),
                        ))
                        .execute(&mut conn)
                        .map_err(|e| e.to_string())
                });
            match stored {
                Ok(_) => {
                    info!("Visual PR {}#{number}: preview stored", req.repo);
                    app_state.visual_prs.clear(&req.repo, number);
                }
                Err(e) => {
                    error!("Visual PR {}#{number}: store failed: {e}", req.repo);
                    app_state.visual_prs.set(
                        &req.repo,
                        number,
                        Transient::Failed {
                            error: format!("generated, but storing failed: {e}"),
                        },
                    );
                }
            }
        }
        Err(e) => {
            error!("Visual PR {}#{number}: generation failed: {e}", req.repo);
            app_state
                .visual_prs
                .set(&req.repo, number, Transient::Failed { error: e });
        }
    }
}

// ============================================================================
// GET /api/admin/visual-prs/{number}/preview.svg?repo=… — serve a stored SVG
// ============================================================================

#[derive(Deserialize)]
pub struct PreviewQuery {
    pub repo: String,
}

pub async fn get_visual_pr_svg(
    State(app_state): State<Arc<AppState>>,
    Path(number): Path<i64>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(q): Query<PreviewQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app_state, &headers, &cookies)?;
    validate_repo(&q.repo)?;
    let mut conn = app_state.conn()?;
    let svg: Option<String> = schema::visual_pr_previews::table
        .filter(schema::visual_pr_previews::repo.eq(&q.repo))
        .filter(schema::visual_pr_previews::pr_number.eq(number))
        .select(schema::visual_pr_previews::svg)
        .first(&mut conn)
        .optional()?;
    match svg {
        Some(svg) => Ok((
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            svg,
        )),
        None => Err(AppError::NotFound("no preview for this PR")),
    }
}

// ============================================================================
// POST /api/admin/visual-prs/{number}/approve — squash-merge via the host's gh
// ============================================================================

pub async fn approve_visual_pr(
    State(app_state): State<Arc<AppState>>,
    Path(number): Path<i64>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<VisualPrApproveRequest>,
) -> Result<Json<VisualPrApproveResponse>, AppError> {
    let admin = require_admin(&app_state, &headers, &cookies)?;
    validate_repo(&req.repo)?;
    require_visual_pr_launcher(&app_state, req.launcher_id, admin.id)?;

    info!(
        "Visual PR {}#{number}: approve requested by {}",
        req.repo, admin.email
    );
    let request_id = Uuid::new_v4();
    let reply = launcher_rpc(
        &app_state,
        req.launcher_id,
        request_id,
        ServerToLauncher::VisualPrApprove {
            request_id,
            repo: req.repo.clone(),
            pr_number: number,
        },
        APPROVE_TIMEOUT_SECS,
    )
    .await?;
    match reply {
        LauncherToServer::VisualPrApproveResult {
            success: true,
            message,
            ..
        } => Ok(Json(VisualPrApproveResponse {
            message: message.unwrap_or_else(|| format!("PR #{number} approved")),
        })),
        LauncherToServer::VisualPrApproveResult { message, .. } => {
            Err(AppError::BadGatewayMessage(format!(
                "gh pr merge failed: {}",
                message.unwrap_or_else(|| "(no detail)".into())
            )))
        }
        _ => Err(AppError::Internal(
            "unexpected launcher reply to VisualPrApprove".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_validation_is_strict() {
        assert!(validate_repo("meawoppl/agent-portal").is_ok());
        assert!(validate_repo("a-b/c_d.e").is_ok());
        assert!(validate_repo("meawoppl").is_err());
        assert!(validate_repo("a/b/c").is_err());
        assert!(validate_repo("a b/c").is_err());
        assert!(validate_repo("-owner/name").is_err());
        assert!(validate_repo("owner/").is_err());
    }

    #[test]
    fn model_validation_is_strict() {
        assert!(validate_model(&None).is_ok());
        assert!(validate_model(&Some("sonnet".into())).is_ok());
        assert!(validate_model(&Some("claude-fable-5".into())).is_ok());
        assert!(validate_model(&Some("-p".into())).is_err());
        assert!(validate_model(&Some("a model".into())).is_err());
        assert!(validate_model(&Some(String::new())).is_err());
    }
}

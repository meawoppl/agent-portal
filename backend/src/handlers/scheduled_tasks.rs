//! Scheduled Task Management Handlers
//!
//! CRUD endpoints for managing scheduled (cron) tasks.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use diesel::prelude::*;
use shared::api::{
    CreateScheduledTaskRequest, ScheduledTaskInfo, ScheduledTaskListResponse,
    UpdateScheduledTaskRequest,
};
use shared::{AgentType, ScheduledTaskConfig, ServerToLauncher};
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    models::{NewScheduledTask, ScheduledTask},
    schema::scheduled_tasks,
    AppState,
};

// ============================================================================
// Internal helpers
// ============================================================================

/// Extract user_id from session cookie (same pattern as proxy_tokens)
async fn get_user_id_from_session(
    app_state: &AppState,
    cookies: &Cookies,
) -> Result<Uuid, StatusCode> {
    if app_state.dev_mode {
        let mut conn = app_state
            .db_pool
            .get()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        use crate::schema::users;
        let user: crate::models::User = users::table
            .filter(users::email.eq("testing@testing.local"))
            .first(&mut conn)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;

        return Ok(user.id);
    }

    let session_cookie = cookies
        .signed(&app_state.cookie_key)
        .get(shared::protocol::SESSION_COOKIE_NAME)
        .ok_or(StatusCode::UNAUTHORIZED)?;

    session_cookie
        .value()
        .parse()
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

/// Convert a ScheduledTask model to a ScheduledTaskInfo API response.
fn task_to_info(t: ScheduledTask) -> ScheduledTaskInfo {
    ScheduledTaskInfo {
        id: t.id,
        name: t.name,
        cron_expression: t.cron_expression,
        timezone: t.timezone,
        hostname: t.hostname,
        working_directory: t.working_directory,
        prompt: t.prompt,
        claude_args: serde_json::from_value(t.claude_args).unwrap_or_default(),
        agent_type: t.agent_type.parse().unwrap_or(AgentType::Claude),
        enabled: t.enabled,
        max_runtime_minutes: t.max_runtime_minutes,
        last_session_id: t.last_session_id,
        last_run_at: t.last_run_at.map(|dt| dt.and_utc().to_rfc3339()),
        next_run_at: t.next_run_at.map(|dt| dt.and_utc().to_rfc3339()),
        created_at: t.created_at.and_utc().to_rfc3339(),
        updated_at: t.updated_at.and_utc().to_rfc3339(),
    }
}

/// Convert a ScheduledTask model to a ScheduledTaskConfig protocol message.
fn task_to_config(t: &ScheduledTask) -> ScheduledTaskConfig {
    ScheduledTaskConfig {
        id: t.id,
        name: t.name.clone(),
        cron_expression: t.cron_expression.clone(),
        timezone: t.timezone.clone(),
        working_directory: t.working_directory.clone(),
        prompt: t.prompt.clone(),
        claude_args: serde_json::from_value(t.claude_args.clone()).unwrap_or_default(),
        agent_type: t.agent_type.parse().unwrap_or(AgentType::Claude),
        enabled: t.enabled,
        max_runtime_minutes: t.max_runtime_minutes,
        last_session_id: t.last_session_id,
    }
}

/// Send ScheduleSync to all connected launchers for a user.
/// Filters tasks by launcher hostname.
fn send_schedule_sync(app_state: &AppState, user_id: Uuid) {
    let tasks: Vec<ScheduledTask> = match app_state.db_pool.get() {
        Ok(mut conn) => scheduled_tasks::table
            .filter(scheduled_tasks::user_id.eq(user_id))
            .filter(scheduled_tasks::enabled.eq(true))
            .load(&mut conn)
            .unwrap_or_default(),
        Err(e) => {
            error!("Failed to get DB connection for ScheduleSync: {}", e);
            return;
        }
    };

    let launchers = app_state.session_manager.get_launchers_for_user(&user_id);
    for launcher in launchers {
        let filtered: Vec<ScheduledTaskConfig> = tasks
            .iter()
            .filter(|t| t.hostname.is_none() || t.hostname.as_deref() == Some(&launcher.hostname))
            .map(task_to_config)
            .collect();

        if app_state.session_manager.send_to_launcher(
            &launcher.launcher_id,
            ServerToLauncher::ScheduleSync { tasks: filtered },
        ) {
            info!(
                "Sent ScheduleSync to launcher '{}' ({})",
                launcher.launcher_name, launcher.launcher_id
            );
        }
    }
}

// ============================================================================
// Core handlers
// ============================================================================

/// GET /api/scheduled-tasks
async fn list_tasks(
    State(app_state): State<Arc<AppState>>,
    user_id: Uuid,
) -> Result<Json<ScheduledTaskListResponse>, StatusCode> {
    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let tasks: Vec<ScheduledTask> = scheduled_tasks::table
        .filter(scheduled_tasks::user_id.eq(user_id))
        .order(scheduled_tasks::created_at.desc())
        .load(&mut conn)
        .map_err(|e| {
            error!("Failed to list scheduled tasks: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let infos: Vec<ScheduledTaskInfo> = tasks.into_iter().map(task_to_info).collect();
    Ok(Json(ScheduledTaskListResponse { tasks: infos }))
}

/// POST /api/scheduled-tasks
async fn create_task(
    State(app_state): State<Arc<AppState>>,
    user_id: Uuid,
    Json(req): Json<CreateScheduledTaskRequest>,
) -> Result<Json<ScheduledTaskInfo>, StatusCode> {
    // Basic cron validation: must have 5 space-separated fields
    let fields: Vec<&str> = req.cron_expression.split_whitespace().collect();
    if fields.len() != 5 {
        warn!("Invalid cron expression: {}", req.cron_expression);
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let new_task = NewScheduledTask {
        user_id,
        name: req.name,
        cron_expression: req.cron_expression,
        timezone: req.timezone,
        hostname: req.hostname,
        working_directory: req.working_directory,
        prompt: req.prompt,
        claude_args: serde_json::to_value(req.claude_args).unwrap_or_default(),
        agent_type: req.agent_type.as_str().to_string(),
        max_runtime_minutes: req.max_runtime_minutes,
    };

    let saved: ScheduledTask = diesel::insert_into(scheduled_tasks::table)
        .values(&new_task)
        .get_result(&mut conn)
        .map_err(|e| {
            error!("Failed to create scheduled task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!("Created scheduled task '{}' ({})", saved.name, saved.id);

    // Notify connected launchers
    send_schedule_sync(&app_state, user_id);

    Ok(Json(task_to_info(saved)))
}

/// PATCH /api/scheduled-tasks/:id
async fn update_task(
    State(app_state): State<Arc<AppState>>,
    user_id: Uuid,
    Path(task_id): Path<Uuid>,
    Json(req): Json<UpdateScheduledTaskRequest>,
) -> Result<Json<ScheduledTaskInfo>, StatusCode> {
    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify ownership
    let existing: ScheduledTask = scheduled_tasks::table
        .filter(scheduled_tasks::id.eq(task_id))
        .filter(scheduled_tasks::user_id.eq(user_id))
        .first(&mut conn)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Validate cron if provided
    if let Some(ref cron) = req.cron_expression {
        let fields: Vec<&str> = cron.split_whitespace().collect();
        if fields.len() != 5 {
            warn!("Invalid cron expression in update: {}", cron);
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    // Apply updates field by field (load-modify-save pattern)
    let name = req.name.unwrap_or(existing.name);
    let cron_expression = req.cron_expression.unwrap_or(existing.cron_expression);
    let timezone = req.timezone.unwrap_or(existing.timezone);
    let hostname = match req.hostname {
        Some(h) => h,
        None => existing.hostname,
    };
    let working_directory = req.working_directory.unwrap_or(existing.working_directory);
    let prompt = req.prompt.unwrap_or(existing.prompt);
    let claude_args = req
        .claude_args
        .map(|args| serde_json::to_value(args).unwrap_or_default())
        .unwrap_or(existing.claude_args);
    let agent_type = req
        .agent_type
        .map(|at| at.as_str().to_string())
        .unwrap_or(existing.agent_type);
    let enabled = req.enabled.unwrap_or(existing.enabled);
    let max_runtime_minutes = req
        .max_runtime_minutes
        .unwrap_or(existing.max_runtime_minutes);

    let updated: ScheduledTask = diesel::update(
        scheduled_tasks::table
            .filter(scheduled_tasks::id.eq(task_id))
            .filter(scheduled_tasks::user_id.eq(user_id)),
    )
    .set((
        scheduled_tasks::name.eq(&name),
        scheduled_tasks::cron_expression.eq(&cron_expression),
        scheduled_tasks::timezone.eq(&timezone),
        scheduled_tasks::hostname.eq(&hostname),
        scheduled_tasks::working_directory.eq(&working_directory),
        scheduled_tasks::prompt.eq(&prompt),
        scheduled_tasks::claude_args.eq(&claude_args),
        scheduled_tasks::agent_type.eq(&agent_type),
        scheduled_tasks::enabled.eq(enabled),
        scheduled_tasks::max_runtime_minutes.eq(max_runtime_minutes),
        scheduled_tasks::updated_at.eq(diesel::dsl::now),
    ))
    .get_result(&mut conn)
    .map_err(|e| {
        error!("Failed to update scheduled task: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    info!("Updated scheduled task '{}' ({})", updated.name, updated.id);

    // Notify connected launchers
    send_schedule_sync(&app_state, user_id);

    Ok(Json(task_to_info(updated)))
}

/// DELETE /api/scheduled-tasks/:id
async fn delete_task(
    State(app_state): State<Arc<AppState>>,
    user_id: Uuid,
    Path(task_id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify ownership
    let task: ScheduledTask = scheduled_tasks::table
        .filter(scheduled_tasks::id.eq(task_id))
        .filter(scheduled_tasks::user_id.eq(user_id))
        .first(&mut conn)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Clear scheduled_task_id on any sessions referencing this task
    use crate::schema::sessions;
    let _ = diesel::update(sessions::table.filter(sessions::scheduled_task_id.eq(task_id)))
        .set(sessions::scheduled_task_id.eq(None::<Uuid>))
        .execute(&mut conn);

    diesel::delete(scheduled_tasks::table.filter(scheduled_tasks::id.eq(task_id)))
        .execute(&mut conn)
        .map_err(|e| {
            error!("Failed to delete scheduled task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!("Deleted scheduled task '{}' ({})", task.name, task_id);

    // Notify connected launchers
    send_schedule_sync(&app_state, user_id);

    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/scheduled-tasks/:id/runs
async fn list_runs(
    State(app_state): State<Arc<AppState>>,
    user_id: Uuid,
    Path(task_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify task ownership
    let _task: ScheduledTask = scheduled_tasks::table
        .filter(scheduled_tasks::id.eq(task_id))
        .filter(scheduled_tasks::user_id.eq(user_id))
        .first(&mut conn)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    use crate::schema::sessions;
    let runs: Vec<crate::models::Session> = sessions::table
        .filter(sessions::scheduled_task_id.eq(task_id))
        .order(sessions::created_at.desc())
        .limit(50)
        .load(&mut conn)
        .map_err(|e| {
            error!("Failed to list runs for task {}: {}", task_id, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(serde_json::to_value(runs).unwrap_or_default()))
}

// ============================================================================
// Wrapper handlers (extract user_id from session cookie)
// ============================================================================

pub async fn list_tasks_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<ScheduledTaskListResponse>, StatusCode> {
    let user_id = get_user_id_from_session(&app_state, &cookies).await?;
    list_tasks(State(app_state), user_id).await
}

pub async fn create_task_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(req): Json<CreateScheduledTaskRequest>,
) -> Result<Json<ScheduledTaskInfo>, StatusCode> {
    let user_id = get_user_id_from_session(&app_state, &cookies).await?;
    create_task(State(app_state), user_id, Json(req)).await
}

pub async fn update_task_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(task_id): Path<Uuid>,
    Json(req): Json<UpdateScheduledTaskRequest>,
) -> Result<Json<ScheduledTaskInfo>, StatusCode> {
    let user_id = get_user_id_from_session(&app_state, &cookies).await?;
    update_task(State(app_state), user_id, Path(task_id), Json(req)).await
}

pub async fn delete_task_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(task_id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let user_id = get_user_id_from_session(&app_state, &cookies).await?;
    delete_task(State(app_state), user_id, Path(task_id)).await
}

pub async fn list_runs_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(task_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let user_id = get_user_id_from_session(&app_state, &cookies).await?;
    list_runs(State(app_state), user_id, Path(task_id)).await
}

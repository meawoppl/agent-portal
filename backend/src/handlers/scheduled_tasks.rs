//! Scheduled Task Management Handlers
//!
//! CRUD endpoints for managing scheduled (cron) tasks.

use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use shared::api::{
    CreateScheduledTaskRequest, ScheduledTaskInfo, ScheduledTaskListResponse,
    UpdateScheduledTaskRequest,
};
use shared::{AgentType, ScheduledTaskConfig, ServerToLauncher};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    auth::CurrentUserId,
    errors::AppError,
    handlers::responses::EmptyResponse,
    models::{NewScheduledTask, ScheduledTask, ScheduledTaskChangeset, Session},
    schema::scheduled_tasks,
    AppState,
};

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
            .filter(|t| t.hostname == launcher.hostname)
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
pub async fn list_tasks_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<ScheduledTaskListResponse>, AppError> {
    let mut conn = app_state.conn()?;

    let tasks: Vec<ScheduledTask> = scheduled_tasks::table
        .filter(scheduled_tasks::user_id.eq(user_id))
        .order(scheduled_tasks::created_at.desc())
        .load(&mut conn)?;

    let infos: Vec<ScheduledTaskInfo> = tasks.into_iter().map(task_to_info).collect();
    Ok(Json(ScheduledTaskListResponse { tasks: infos }))
}

/// POST /api/scheduled-tasks
pub async fn create_task_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(req): Json<CreateScheduledTaskRequest>,
) -> Result<Json<ScheduledTaskInfo>, AppError> {
    // Basic cron validation: must have 5 space-separated fields
    let fields: Vec<&str> = req.cron_expression.split_whitespace().collect();
    if fields.len() != 5 {
        warn!("Invalid cron expression: {}", req.cron_expression);
        return Err(AppError::BadRequest("Invalid cron expression"));
    }

    let mut conn = app_state.conn()?;

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
        .get_result(&mut conn)?;

    info!("Created scheduled task '{}' ({})", saved.name, saved.id);

    // Notify connected launchers
    send_schedule_sync(&app_state, user_id);

    Ok(Json(task_to_info(saved)))
}

/// PATCH /api/scheduled-tasks/:id
pub async fn update_task_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(task_id): Path<Uuid>,
    Json(req): Json<UpdateScheduledTaskRequest>,
) -> Result<Json<ScheduledTaskInfo>, AppError> {
    let mut conn = app_state.conn()?;

    // Verify ownership
    scheduled_tasks::table
        .filter(scheduled_tasks::id.eq(task_id))
        .filter(scheduled_tasks::user_id.eq(user_id))
        .select(scheduled_tasks::id)
        .first::<Uuid>(&mut conn)
        .map_err(|_| AppError::NotFound("scheduled task"))?;

    // Validate cron if provided
    if let Some(ref cron) = req.cron_expression {
        let fields: Vec<&str> = cron.split_whitespace().collect();
        if fields.len() != 5 {
            warn!("Invalid cron expression in update: {}", cron);
            return Err(AppError::BadRequest("Invalid cron expression"));
        }
    }

    let changeset = ScheduledTaskChangeset {
        name: req.name,
        cron_expression: req.cron_expression,
        timezone: req.timezone,
        hostname: req.hostname,
        working_directory: req.working_directory,
        prompt: req.prompt,
        claude_args: req
            .claude_args
            .map(|args| serde_json::to_value(args).unwrap_or_default()),
        agent_type: req.agent_type.map(|at| at.as_str().to_string()),
        enabled: req.enabled,
        max_runtime_minutes: req.max_runtime_minutes,
    };

    let updated: ScheduledTask = diesel::update(
        scheduled_tasks::table
            .filter(scheduled_tasks::id.eq(task_id))
            .filter(scheduled_tasks::user_id.eq(user_id)),
    )
    .set((&changeset, scheduled_tasks::updated_at.eq(diesel::dsl::now)))
    .get_result(&mut conn)?;

    info!("Updated scheduled task '{}' ({})", updated.name, updated.id);

    // Notify connected launchers
    send_schedule_sync(&app_state, user_id);

    Ok(Json(task_to_info(updated)))
}

/// DELETE /api/scheduled-tasks/:id
pub async fn delete_task_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(task_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;

    // Verify ownership
    let task: ScheduledTask = scheduled_tasks::table
        .filter(scheduled_tasks::id.eq(task_id))
        .filter(scheduled_tasks::user_id.eq(user_id))
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("scheduled task"))?;

    // Clear scheduled_task_id on any sessions referencing this task
    use crate::schema::sessions;
    let _ = diesel::update(sessions::table.filter(sessions::scheduled_task_id.eq(task_id)))
        .set(sessions::scheduled_task_id.eq(None::<Uuid>))
        .execute(&mut conn);

    diesel::delete(scheduled_tasks::table.filter(scheduled_tasks::id.eq(task_id)))
        .execute(&mut conn)?;

    info!("Deleted scheduled task '{}' ({})", task.name, task_id);

    // Notify connected launchers
    send_schedule_sync(&app_state, user_id);

    Ok(EmptyResponse::NO_CONTENT)
}

/// GET /api/scheduled-tasks/:id/runs
pub async fn list_runs_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(task_id): Path<Uuid>,
) -> Result<Json<Vec<Session>>, AppError> {
    let mut conn = app_state.conn()?;

    // Verify task ownership
    scheduled_tasks::table
        .filter(scheduled_tasks::id.eq(task_id))
        .filter(scheduled_tasks::user_id.eq(user_id))
        .select(scheduled_tasks::id)
        .first::<Uuid>(&mut conn)
        .map_err(|_| AppError::NotFound("scheduled task"))?;

    use crate::schema::sessions;
    let runs: Vec<Session> = sessions::table
        .filter(sessions::scheduled_task_id.eq(task_id))
        .order(sessions::created_at.desc())
        .limit(50)
        .load(&mut conn)?;

    Ok(Json(runs))
}

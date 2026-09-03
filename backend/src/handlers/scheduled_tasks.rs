//! Scheduled Task Management Handlers
//!
//! CRUD endpoints for managing scheduled (cron) tasks.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use croner::Cron;
use diesel::prelude::*;
use shared::api::{
    CreateScheduledTaskRequest, ScheduledTaskInfo, ScheduledTaskListResponse,
    ScheduledTaskOccurrence, UpcomingScheduledTasksResponse, UpdateScheduledTaskRequest,
};
use shared::{AgentType, ScheduledTaskConfig, ScheduledTaskFields, ServerToLauncher};
use std::{str::FromStr, sync::Arc};
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

/// Extract the shared scheduled-task fields from a ScheduledTask model.
fn task_to_fields(t: &ScheduledTask) -> ScheduledTaskFields {
    ScheduledTaskFields {
        name: t.name.clone(),
        cron_expression: t.cron_expression.clone(),
        timezone: t.timezone.clone(),
        working_directory: t.working_directory.clone(),
        prompt: t.prompt.clone(),
        claude_args: serde_json::from_value(t.claude_args.clone()).unwrap_or_default(),
        agent_type: t.agent_type.parse().unwrap_or(AgentType::Claude),
        max_runtime_minutes: t.max_runtime_minutes,
        session_mode: t.session_mode.parse().unwrap_or_default(),
    }
}

/// Convert a ScheduledTask model to a ScheduledTaskInfo API response.
fn task_to_info(t: ScheduledTask) -> ScheduledTaskInfo {
    ScheduledTaskInfo {
        id: t.id,
        fields: task_to_fields(&t),
        hostname: t.hostname,
        enabled: t.enabled,
        last_session_id: t.last_session_id,
        last_run_at: t.last_run_at.map(|dt| dt.and_utc().to_rfc3339()),
        created_at: t.created_at.and_utc().to_rfc3339(),
        updated_at: t.updated_at.and_utc().to_rfc3339(),
    }
}

fn validate_cron_expression(cron_expression: &str) -> Result<(), AppError> {
    let cron_fields: Vec<&str> = cron_expression.split_whitespace().collect();
    if cron_fields.len() != 5 {
        return Err(AppError::BadRequest("Invalid cron expression"));
    }
    Ok(())
}

const UPCOMING_WINDOW_HOURS: i64 = 72;
const MAX_UPCOMING_OCCURRENCES: usize = 10_000;

fn upcoming_for_task(
    task: &ScheduledTask,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    remaining: usize,
) -> Vec<ScheduledTaskOccurrence> {
    let Ok(cron) = Cron::from_str(&task.cron_expression) else {
        return Vec::new();
    };
    let canonical = shared::timezone::canonicalize_timezone(&task.timezone);
    let timezone = canonical.parse::<chrono_tz::Tz>().unwrap_or(chrono_tz::UTC);
    let mut cursor = starts_at.with_timezone(&timezone);
    let mut occurrences = Vec::new();

    while occurrences.len() < remaining {
        let Ok(next) = cron.find_next_occurrence(&cursor, false) else {
            break;
        };
        let next_utc = next.with_timezone(&Utc);
        if next_utc > ends_at {
            break;
        }
        occurrences.push(ScheduledTaskOccurrence {
            task_id: task.id,
            task_name: task.name.clone(),
            hostname: task.hostname.clone(),
            agent_type: task.agent_type.parse().unwrap_or(AgentType::Claude),
            scheduled_for: next_utc.to_rfc3339(),
        });
        cursor = next;
    }

    occurrences
}

/// Convert a ScheduledTask model to a ScheduledTaskConfig protocol message.
pub(crate) fn task_to_config(t: &ScheduledTask) -> ScheduledTaskConfig {
    ScheduledTaskConfig {
        id: t.id,
        fields: task_to_fields(t),
        enabled: t.enabled,
        last_session_id: t.last_session_id,
    }
}

/// Load a user's enabled scheduled tasks. Returns None if no DB connection.
fn load_enabled_tasks(app_state: &AppState, user_id: Uuid) -> Option<Vec<ScheduledTask>> {
    match app_state.db_pool.get() {
        Ok(mut conn) => Some(
            scheduled_tasks::table
                .filter(scheduled_tasks::user_id.eq(user_id))
                .filter(scheduled_tasks::enabled.eq(true))
                .load(&mut conn)
                .unwrap_or_default(),
        ),
        Err(e) => {
            error!("Failed to get DB connection for ScheduleSync: {}", e);
            None
        }
    }
}

/// Send ScheduleSync to all connected launchers for a user.
/// Filters tasks by launcher hostname.
fn send_schedule_sync(app_state: &AppState, user_id: Uuid) {
    let Some(tasks) = load_enabled_tasks(app_state, user_id) else {
        return;
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

/// Send the initial ScheduleSync to a single newly-registered launcher.
/// Filters the user's enabled tasks by the launcher's hostname and, unlike
/// the broadcast in `send_schedule_sync`, sends nothing when no tasks match.
pub(crate) fn send_initial_schedule_sync(
    app_state: &AppState,
    user_id: Uuid,
    launcher_id: Uuid,
    hostname: &str,
    launcher_name: &str,
) {
    let Some(tasks) = load_enabled_tasks(app_state, user_id) else {
        return;
    };

    let task_configs: Vec<ScheduledTaskConfig> = tasks
        .iter()
        .filter(|t| t.hostname == hostname)
        .map(task_to_config)
        .collect();

    if task_configs.is_empty() {
        return;
    }

    let count = task_configs.len();
    if app_state.session_manager.send_to_launcher(
        &launcher_id,
        ServerToLauncher::ScheduleSync {
            tasks: task_configs,
        },
    ) {
        info!(
            "Sent initial ScheduleSync with {} tasks to launcher '{}'",
            count, launcher_name
        );
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

/// GET /api/scheduled-tasks/upcoming — enabled firings in the next 72 hours.
pub async fn upcoming_tasks_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<UpcomingScheduledTasksResponse>, AppError> {
    let mut conn = app_state.conn()?;
    let tasks: Vec<ScheduledTask> = scheduled_tasks::table
        .filter(scheduled_tasks::user_id.eq(user_id))
        .filter(scheduled_tasks::enabled.eq(true))
        .load(&mut conn)?;
    let starts_at = Utc::now();
    let ends_at = starts_at + Duration::hours(UPCOMING_WINDOW_HOURS);
    let mut occurrences = Vec::new();

    for task in &tasks {
        let remaining = (MAX_UPCOMING_OCCURRENCES + 1).saturating_sub(occurrences.len());
        if remaining == 0 {
            break;
        }
        occurrences.extend(upcoming_for_task(task, starts_at, ends_at, remaining));
    }
    occurrences.sort_by(|a, b| a.scheduled_for.cmp(&b.scheduled_for));
    let truncated = occurrences.len() > MAX_UPCOMING_OCCURRENCES;
    occurrences.truncate(MAX_UPCOMING_OCCURRENCES);

    Ok(Json(UpcomingScheduledTasksResponse {
        starts_at: starts_at.to_rfc3339(),
        ends_at: ends_at.to_rfc3339(),
        occurrences,
        truncated,
    }))
}

/// POST /api/scheduled-tasks
pub async fn create_task_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(req): Json<CreateScheduledTaskRequest>,
) -> Result<Json<ScheduledTaskInfo>, AppError> {
    if let Err(err) = validate_cron_expression(&req.fields.cron_expression) {
        warn!("Invalid cron expression: {}", req.fields.cron_expression);
        return Err(err);
    }

    let mut conn = app_state.conn()?;

    let new_task = NewScheduledTask {
        user_id,
        name: req.fields.name,
        cron_expression: req.fields.cron_expression,
        // Normalize abbreviations (PST/EST/…) to IANA so the launcher's
        // chrono_tz parse succeeds instead of silently using UTC (#1064).
        timezone: shared::timezone::canonicalize_timezone(&req.fields.timezone),
        hostname: req.hostname,
        working_directory: req.fields.working_directory,
        prompt: req.fields.prompt,
        claude_args: serde_json::to_value(req.fields.claude_args).unwrap_or_default(),
        agent_type: req.fields.agent_type.as_str().to_string(),
        max_runtime_minutes: req.fields.max_runtime_minutes,
        session_mode: req.fields.session_mode.as_str().to_string(),
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
        if let Err(err) = validate_cron_expression(cron) {
            warn!("Invalid cron expression in update: {}", cron);
            return Err(err);
        }
    }

    let changeset = ScheduledTaskChangeset {
        name: req.name,
        cron_expression: req.cron_expression,
        // Normalize abbreviations to IANA on update too (#1064).
        timezone: req
            .timezone
            .map(|tz| shared::timezone::canonicalize_timezone(&tz)),
        hostname: req.hostname,
        working_directory: req.working_directory,
        prompt: req.prompt,
        claude_args: req
            .claude_args
            .map(|args| serde_json::to_value(args).unwrap_or_default()),
        agent_type: req.agent_type.map(|at| at.as_str().to_string()),
        enabled: req.enabled,
        max_runtime_minutes: req.max_runtime_minutes,
        session_mode: req.session_mode.map(|m| m.as_str().to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn task(cron_expression: &str, timezone: &str) -> ScheduledTask {
        let now = Utc::now().naive_utc();
        ScheduledTask {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            name: "Morning check".to_string(),
            cron_expression: cron_expression.to_string(),
            timezone: timezone.to_string(),
            hostname: "workstation".to_string(),
            working_directory: "/tmp".to_string(),
            prompt: "check".to_string(),
            claude_args: serde_json::Value::Array(Vec::new()),
            agent_type: "codex".to_string(),
            enabled: true,
            max_runtime_minutes: 30,
            last_session_id: None,
            last_run_at: None,
            created_at: now,
            updated_at: now,
            session_mode: "fresh".to_string(),
        }
    }

    #[test]
    fn invalid_cron_validation_is_bad_request() {
        let err = validate_cron_expression("* * *").unwrap_err();
        assert!(matches!(
            err,
            AppError::BadRequest("Invalid cron expression")
        ));
    }

    #[test]
    fn valid_cron_validation_accepts_five_fields() {
        validate_cron_expression("*/5 * * * *").unwrap();
    }

    #[test]
    fn upcoming_occurrences_are_bounded_and_chronological() {
        let starts_at = DateTime::parse_from_rfc3339("2026-09-02T12:34:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let occurrences = upcoming_for_task(
            &task("0 * * * *", "UTC"),
            starts_at,
            starts_at + Duration::hours(3),
            10,
        );
        let times = occurrences
            .iter()
            .map(|occurrence| occurrence.scheduled_for.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            times,
            [
                "2026-09-02T13:00:00+00:00",
                "2026-09-02T14:00:00+00:00",
                "2026-09-02T15:00:00+00:00",
            ]
        );
    }

    #[test]
    fn upcoming_occurrences_apply_the_task_timezone() {
        let starts_at = DateTime::parse_from_rfc3339("2026-09-02T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let occurrences = upcoming_for_task(
            &task("0 9 * * *", "America/Los_Angeles"),
            starts_at,
            starts_at + Duration::hours(24),
            10,
        );
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].scheduled_for, "2026-09-02T16:00:00+00:00");
    }
}

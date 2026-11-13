//! Small, durable audit trail for identity, computers and secrets only.
use crate::{
    errors::AppError,
    schema::{audit_events as events, users},
    AppState,
};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use shared::audit::{AuditCategory, AuditDetails, AuditEvent, AuditPage, AuditQuery};
use std::sync::Arc;
use tower_cookies::Cookies;
use uuid::Uuid;

#[derive(Clone, Copy)]
pub enum Action {
    Login,
    Logout,
    Verification,
    DeviceApproved,
    DeviceDenied,
    ComputerRegistered,
    ComputerUpdateRequested,
    ComputerRestartRequested,
    AgentInstallRequested,
    AgentInstallResult,
    AgentLoginStarted,
    AgentLoginResult,
    AgentLoginCancelled,
}
impl Action {
    fn identity(self) -> (AuditCategory, &'static str, &'static str) {
        use Action::*;
        match self {
            Login => (AuditCategory::Identity, "login", "user"),
            Logout => (AuditCategory::Identity, "logout", "user"),
            Verification => (AuditCategory::Identity, "portal_access.checked", "user"),
            DeviceApproved => (AuditCategory::Identity, "device.approved", "user"),
            DeviceDenied => (AuditCategory::Identity, "device.denied", "user"),
            ComputerRegistered => (AuditCategory::Computers, "computer.registered", "computer"),
            ComputerUpdateRequested => (
                AuditCategory::Computers,
                "computer.update.requested",
                "computer",
            ),
            ComputerRestartRequested => (
                AuditCategory::Computers,
                "computer.restart.requested",
                "computer",
            ),
            AgentInstallRequested => (
                AuditCategory::Computers,
                "agent.install.requested",
                "computer",
            ),
            AgentInstallResult => (AuditCategory::Computers, "agent.install.result", "computer"),
            AgentLoginStarted => (AuditCategory::Computers, "agent.login.started", "computer"),
            AgentLoginResult => (AuditCategory::Computers, "agent.login.result", "computer"),
            AgentLoginCancelled => (
                AuditCategory::Computers,
                "agent.login.cancelled",
                "computer",
            ),
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct Context {
    pub session: Option<Uuid>,
    pub launcher: Option<Uuid>,
    pub operation: Option<Uuid>,
    pub agent: bool,
}

/// Call within the business transaction for mutations; before releasing values
/// or sending external commands. No secrets or request bodies are accepted.
pub fn record(
    conn: &mut PgConnection,
    actor: Option<Uuid>,
    target: Option<Uuid>,
    action: Action,
    outcome: &str,
    operation: Option<Uuid>,
) -> Result<(), AppError> {
    let (category, event_type, target_kind) = action.identity();
    write(
        conn,
        category,
        event_type,
        actor,
        target_kind,
        target,
        outcome,
        Context {
            operation,
            launcher: (category == AuditCategory::Computers)
                .then_some(target)
                .flatten(),
            ..Default::default()
        },
    )
}
#[allow(clippy::too_many_arguments)]
pub fn write(
    conn: &mut PgConnection,
    category: AuditCategory,
    event_type: &str,
    actor: Option<Uuid>,
    target_kind: &str,
    target: Option<Uuid>,
    outcome: &str,
    context: Context,
) -> Result<(), AppError> {
    diesel::insert_into(events::table)
        .values((
            events::id.eq(Uuid::new_v4()),
            events::category.eq(category.as_str()),
            events::event_type.eq(event_type),
            events::actor_user_id.eq(actor),
            events::actor_kind.eq(if context.agent {
                "agent"
            } else if actor.is_some() {
                "user"
            } else {
                "unknown"
            }),
            events::target_kind.eq(target_kind),
            events::target_id.eq(target),
            events::session_id.eq(context.session),
            events::launcher_id.eq(context.launcher),
            events::operation_id.eq(context.operation),
            events::outcome.eq(outcome),
        ))
        .execute(conn)?;
    Ok(())
}
/// Observation after an external result cannot roll that result back. Failure
/// is explicit in operational logs and never contains input/error payloads.
pub fn observe(
    state: &AppState,
    actor: Option<Uuid>,
    target: Option<Uuid>,
    action: Action,
    outcome: &str,
    operation: Option<Uuid>,
) {
    let result = state
        .conn()
        .and_then(|mut conn| record(&mut conn, actor, target, action, outcome, operation));
    if result.is_err() {
        tracing::error!(
            "{} audit observation was not saved",
            crate::markers::AUDIT_PERSIST_FAILED
        );
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = events)]
struct Row {
    id: Uuid,
    occurred_at: DateTime<Utc>,
    category: String,
    event_type: String,
    actor_kind: String,
    actor_user_id: Option<Uuid>,
    owner_user_id: Option<Uuid>,
    session_id: Option<Uuid>,
    launcher_id: Option<Uuid>,
    target_kind: String,
    target_id: Option<Uuid>,
    operation_id: Option<Uuid>,
    outcome: String,
    details: serde_json::Value,
}
fn cursor(query: &AuditQuery) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    match (&query.before_time, query.before_id) {
        (None, None) => Ok(None),
        (Some(time), Some(id)) => Ok(Some((
            DateTime::parse_from_rfc3339(time)
                .map_err(|_| AppError::BadRequest("Invalid audit cursor"))?
                .with_timezone(&Utc),
            id,
        ))),
        _ => Err(AppError::BadRequest("Audit cursor requires time and ID")),
    }
}
fn page(
    conn: &mut PgConnection,
    query: AuditQuery,
    retention_days: Option<u32>,
) -> Result<AuditPage, AppError> {
    let before = cursor(&query)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 100) as i64;
    let mut q = events::table
        .left_join(users::table.on(events::actor_user_id.eq(users::id.nullable())))
        .into_boxed();
    if let Some(category) = query.category {
        q = q.filter(events::category.eq(category.as_str()));
    }
    if let Some(actor) = query.actor {
        q = q.filter(events::actor_user_id.eq(actor));
    }
    if let Some((time, id)) = before {
        q = q.filter(
            events::occurred_at
                .lt(time)
                .or(events::occurred_at.eq(time).and(events::id.lt(id))),
        );
    }
    let mut rows = q
        .order((events::occurred_at.desc(), events::id.desc()))
        .limit(limit + 1)
        .select((Row::as_select(), users::email.nullable()))
        .load::<(Row, Option<String>)>(conn)?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let events = rows
        .into_iter()
        .map(|(r, email)| {
            Ok(AuditEvent {
                id: r.id,
                occurred_at: r.occurred_at.to_rfc3339(),
                category: r.category,
                event_type: r.event_type,
                actor_kind: r.actor_kind,
                actor_user_id: r.actor_user_id,
                actor_email: email,
                owner_user_id: r.owner_user_id,
                session_id: r.session_id,
                launcher_id: r.launcher_id,
                target_kind: r.target_kind,
                target_id: r.target_id,
                operation_id: r.operation_id,
                outcome: r.outcome,
                details: serde_json::from_value::<AuditDetails>(r.details)
                    .map_err(|_| AppError::Internal("Invalid audit metadata".into()))?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(AuditPage {
        events,
        has_more,
        retention_days,
    })
}
pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditPage>, AppError> {
    crate::handlers::admin::require_admin(&state, &headers, &cookies)?;
    // Sync Diesel work belongs on the blocking pool, including large histories.
    tokio::task::spawn_blocking(move || {
        page(&mut *state.conn()?, query, state.audit_retention_days)
    })
    .await
    .map_err(|_| AppError::Internal("Audit query task failed".into()))?
    .map(Json)
}

pub fn parse_retention(value: &str) -> Result<Option<u32>, String> {
    if value == "unlimited" {
        return Ok(None);
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0 && *n <= 36500)
        .map(Some)
        .ok_or_else(|| "PORTAL_AUDIT_RETENTION_DAYS must be 1–36500 or unlimited".into())
}
fn prune(conn: &mut PgConnection, days: u32) -> Result<usize, AppError> {
    let cutoff = Utc::now() - chrono::Duration::days(i64::from(days));
    conn.transaction(|conn| {
        let ids = events::table
            .filter(events::occurred_at.lt(cutoff))
            .order((events::occurred_at.asc(), events::id.asc()))
            .select(events::id)
            .limit(1000)
            .load::<Uuid>(conn)?;
        let count = diesel::delete(events::table.filter(events::id.eq_any(ids))).execute(conn)?;
        // The old secrets-only trail shares the same retention policy.
        use crate::schema::secret_audit_events as legacy;
        let ids = legacy::table
            .filter(legacy::created_at.lt(cutoff))
            .select(legacy::id)
            .limit(1000)
            .load::<Uuid>(conn)?;
        diesel::delete(legacy::table.filter(legacy::id.eq_any(ids))).execute(conn)?;
        Ok(count)
    })
}
pub async fn sweep(state: Arc<AppState>) {
    let Some(days) = state.audit_retention_days else {
        return;
    };
    let result = tokio::task::spawn_blocking(move || prune(&mut *state.conn()?, days)).await;
    if !matches!(result, Ok(Ok(_))) {
        tracing::error!(
            "{} audit retention will retry",
            crate::markers::AUDIT_PERSIST_FAILED
        );
    }
}

/// Repeated terminal polls must not grow the audit trail. Persist one terminal
/// result per flow; use a DB key rather than a growing in-memory tracking map.
pub fn computer_login_result(
    state: &AppState,
    actor: Uuid,
    launcher: Uuid,
    flow: Uuid,
    success: bool,
) {
    let result = state.conn().and_then(|mut conn| {
        diesel::insert_into(events::table)
            .values((
                events::id.eq(Uuid::new_v5(&flow, b"audit:agent-login-result")),
                events::category.eq("computers"),
                events::event_type.eq("agent.login.result"),
                events::actor_user_id.eq(Some(actor)),
                events::launcher_id.eq(Some(launcher)),
                events::target_kind.eq("computer"),
                events::target_id.eq(Some(launcher)),
                events::operation_id.eq(Some(flow)),
                events::outcome.eq(if success { "success" } else { "failed" }),
            ))
            .on_conflict(events::id)
            .do_nothing()
            .execute(&mut conn)?;
        Ok(())
    });
    if result.is_err() {
        tracing::error!(
            "{} computer login result was not saved",
            crate::markers::AUDIT_PERSIST_FAILED
        );
    }
}

#[cfg(test)]
mod tests;

/// Persist a request before an external computer action without blocking the
/// WebSocket executor while waiting for a database connection.
pub async fn required(
    state: &Arc<AppState>,
    actor: Option<Uuid>,
    target: Option<Uuid>,
    action: Action,
    outcome: &'static str,
    operation: Option<Uuid>,
) -> Result<(), AppError> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        record(
            &mut *state.conn()?,
            actor,
            target,
            action,
            outcome,
            operation,
        )
    })
    .await
    .map_err(|_| AppError::Internal("Audit write task failed".into()))?
}

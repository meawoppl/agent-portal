use chrono::{DateTime, Utc};
use diesel::prelude::*;
use shared::{
    ContinuationConfig, PortalMessage, ServerToClient, ServerToLauncher,
    SessionLimitContinuationFields,
};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::{SessionId, SessionManager};
use crate::db::DbPool;
use crate::models::{NewMessage, NewSessionContinuation, Session, SessionContinuation};
use crate::AppState;

const STATUS_PENDING: &str = "pending";
const STATUS_SCHEDULED: &str = "scheduled";
const STATUS_FIRED: &str = "fired";
const STATUS_DROPPED: &str = "dropped";

pub fn handle_session_limit_reached(
    app_state: &AppState,
    session_manager: &SessionManager,
    session_key: &Option<SessionId>,
    db_session_id: Option<Uuid>,
    db_pool: &DbPool,
    fields: SessionLimitContinuationFields,
) {
    let Some(current_session_id) = db_session_id else {
        warn!("Ignoring SessionLimitReached before registration");
        return;
    };
    if current_session_id != fields.session_id {
        warn!(
            "SessionLimitReached session_id mismatch: {} != {}",
            fields.session_id, current_session_id
        );
        return;
    }

    let reset_at = match DateTime::parse_from_rfc3339(&fields.reset_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(e) => {
            warn!(
                "Ignoring SessionLimitReached with unparsable reset_at '{}': {}",
                fields.reset_at, e
            );
            return;
        }
    };

    let Ok(mut conn) = db_pool.get() else {
        error!("Failed to get DB connection for SessionLimitReached");
        return;
    };

    use crate::schema::{session_continuations, sessions};
    let session = match sessions::table
        .find(current_session_id)
        .first::<Session>(&mut conn)
    {
        Ok(session) => session,
        Err(e) => {
            warn!(
                "Ignoring SessionLimitReached for unknown session {}: {}",
                current_session_id, e
            );
            return;
        }
    };

    let Some(launcher_id) = session.launcher_id else {
        warn!(
            "Ignoring SessionLimitReached for session {} without launcher_id",
            current_session_id
        );
        return;
    };

    let new_continuation = NewSessionContinuation {
        session_id: current_session_id,
        user_id: session.user_id,
        launcher_id,
        reset_at,
        prompt: fields.prompt,
        status: STATUS_PENDING.to_string(),
        source_message: Some(fields.source_message),
    };

    let continuation = match diesel::insert_into(session_continuations::table)
        .values(&new_continuation)
        .get_result::<SessionContinuation>(&mut conn)
    {
        Ok(row) => row,
        Err(e) => {
            error!(
                "Failed to create session continuation for {}: {}",
                current_session_id, e
            );
            return;
        }
    };

    let portal = PortalMessage::continuation_prompt(
        continuation.id,
        continuation.reset_at.to_rfc3339(),
        continuation.status.clone(),
        continuation.source_message.clone().unwrap_or_default(),
    );
    let inserted = insert_portal_message(&mut conn, &session, &portal);
    let row_created_at = inserted.as_ref().map(|m| m.created_at_iso());
    let meta = inserted.map(|m| m.portal_meta(None));

    if let Some(key) = session_key {
        session_manager.broadcast_to_web_clients(
            key,
            ServerToClient::AgentOutput {
                content: portal.to_json(),
                sender_user_id: None,
                sender_name: None,
                agent_type: session.agent_type.parse().unwrap_or_default(),
                created_at: row_created_at,
                origin: None,
                meta,
            },
        );
    }

    info!(
        "Created session continuation {} for session {} at {}",
        continuation.id, current_session_id, continuation.reset_at
    );

    sync_continuations_for_launcher(app_state, launcher_id, session.user_id);
}

pub fn schedule_limit_continuation(
    app_state: &AppState,
    session_manager: &SessionManager,
    user_id: Uuid,
    session_id: Uuid,
    continuation_id: Uuid,
) {
    let Ok(mut conn) = app_state.db_pool.get() else {
        return;
    };

    use crate::schema::session_continuations;
    let continuation = match diesel::update(
        session_continuations::table
            .filter(session_continuations::id.eq(continuation_id))
            .filter(session_continuations::session_id.eq(session_id))
            .filter(session_continuations::user_id.eq(user_id))
            .filter(session_continuations::status.eq(STATUS_PENDING)),
    )
    .set((
        session_continuations::status.eq(STATUS_SCHEDULED),
        session_continuations::scheduled_at.eq(diesel::dsl::now),
        session_continuations::updated_at.eq(diesel::dsl::now),
    ))
    .get_result::<SessionContinuation>(&mut conn)
    {
        Ok(row) => row,
        Err(e) => {
            warn!(
                "Failed to schedule continuation {} for session {}: {}",
                continuation_id, session_id, e
            );
            session_manager.broadcast_to_web_clients(
                &session_id.to_string(),
                ServerToClient::ContinuationStatus {
                    continuation_id,
                    status: "failed".to_string(),
                    message: Some("Unable to schedule continuation".to_string()),
                },
            );
            return;
        }
    };

    session_manager.broadcast_to_web_clients(
        &session_id.to_string(),
        ServerToClient::ContinuationStatus {
            continuation_id,
            status: STATUS_SCHEDULED.to_string(),
            message: Some("Continuation scheduled".to_string()),
        },
    );
    sync_continuations_for_launcher(app_state, continuation.launcher_id, user_id);
}

pub fn mark_continuation_fired(
    app_state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
    continuation_id: Uuid,
    session_id: Uuid,
) {
    update_terminal_status(
        app_state,
        launcher_id,
        user_id,
        continuation_id,
        session_id,
        STATUS_FIRED,
        None,
    );
}

pub fn mark_continuation_dropped(
    app_state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
    continuation_id: Uuid,
    session_id: Uuid,
    reason: String,
) {
    update_terminal_status(
        app_state,
        launcher_id,
        user_id,
        continuation_id,
        session_id,
        STATUS_DROPPED,
        Some(reason),
    );
}

fn update_terminal_status(
    app_state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
    continuation_id: Uuid,
    session_id: Uuid,
    status: &str,
    reason: Option<String>,
) {
    let Ok(mut conn) = app_state.db_pool.get() else {
        return;
    };

    use crate::schema::{session_continuations, sessions};
    let updated = diesel::update(
        session_continuations::table
            .filter(session_continuations::id.eq(continuation_id))
            .filter(session_continuations::session_id.eq(session_id))
            .filter(session_continuations::user_id.eq(user_id))
            .filter(session_continuations::launcher_id.eq(launcher_id)),
    )
    .set((
        session_continuations::status.eq(status),
        session_continuations::last_error.eq(reason.clone()),
        session_continuations::updated_at.eq(diesel::dsl::now),
        session_continuations::fired_at.eq(if status == STATUS_FIRED {
            Some(chrono::Utc::now().naive_utc())
        } else {
            None
        }),
        session_continuations::dropped_at.eq(if status == STATUS_DROPPED {
            Some(chrono::Utc::now().naive_utc())
        } else {
            None
        }),
    ))
    .get_result::<SessionContinuation>(&mut conn);

    match updated {
        Ok(row) => {
            app_state.session_manager.broadcast_to_web_clients(
                &session_id.to_string(),
                ServerToClient::ContinuationStatus {
                    continuation_id,
                    status: status.to_string(),
                    message: reason.clone(),
                },
            );

            if status == STATUS_DROPPED {
                if let Ok(session) = sessions::table.find(session_id).first::<Session>(&mut conn) {
                    let portal = PortalMessage::text(
                        "Continuation was not sent because the local Claude process was no longer running when the session limit reset."
                            .to_string(),
                    );
                    let inserted = insert_portal_message(&mut conn, &session, &portal);
                    let row_created_at = inserted.as_ref().map(|m| m.created_at_iso());
                    let meta = inserted.map(|m| m.portal_meta(None));
                    app_state.session_manager.broadcast_to_web_clients(
                        &session_id.to_string(),
                        ServerToClient::AgentOutput {
                            content: portal.to_json(),
                            sender_user_id: None,
                            sender_name: None,
                            agent_type: session.agent_type.parse().unwrap_or_default(),
                            created_at: row_created_at,
                            origin: None,
                            meta,
                        },
                    );
                }
            }

            sync_continuations_for_launcher(app_state, row.launcher_id, user_id);
        }
        Err(e) => warn!(
            "Failed to mark continuation {} {} for session {}: {}",
            continuation_id, status, session_id, e
        ),
    }
}

pub fn sync_continuations_for_launcher(app_state: &AppState, launcher_id: Uuid, user_id: Uuid) {
    let continuations = load_scheduled_continuations(app_state, launcher_id, user_id);
    let msg = ServerToLauncher::ContinuationSync { continuations };
    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, msg)
    {
        warn!(
            "Failed to sync continuations to launcher {} for user {}",
            launcher_id, user_id
        );
    }
}

pub fn load_scheduled_continuations(
    app_state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
) -> Vec<ContinuationConfig> {
    let Ok(mut conn) = app_state.db_pool.get() else {
        return Vec::new();
    };

    use crate::schema::session_continuations;
    session_continuations::table
        .filter(session_continuations::launcher_id.eq(launcher_id))
        .filter(session_continuations::user_id.eq(user_id))
        .filter(session_continuations::status.eq(STATUS_SCHEDULED))
        .order(session_continuations::reset_at.asc())
        .load::<SessionContinuation>(&mut conn)
        .unwrap_or_default()
        .into_iter()
        .map(|row| ContinuationConfig {
            id: row.id,
            session_id: row.session_id,
            reset_at: row.reset_at.to_rfc3339(),
            prompt: row.prompt,
        })
        .collect()
}

/// Insert a portal-role message and return the persisted row, so callers can
/// derive both the server `created_at` and the typed `PortalMeta` sidecar
/// (`source = Portal`) for the live broadcast (#portal-meta).
fn insert_portal_message(
    conn: &mut diesel::PgConnection,
    session: &Session,
    portal: &PortalMessage,
) -> Option<crate::models::Message> {
    use crate::schema::messages;
    let new_message = NewMessage {
        session_id: session.id,
        role: "portal".to_string(),
        content: serde_json::to_string(&portal.to_json()).unwrap_or_default(),
        user_id: session.user_id,
        agent_type: session.agent_type.clone(),
        provenance_kind: None,
        provenance_session_id: None,
        provenance_agent_type: None,
    };
    match diesel::insert_into(messages::table)
        .values(&new_message)
        .get_result::<crate::models::Message>(conn)
    {
        Ok(inserted) => Some(inserted),
        Err(e) => {
            error!("Failed to store continuation portal message: {}", e);
            None
        }
    }
}

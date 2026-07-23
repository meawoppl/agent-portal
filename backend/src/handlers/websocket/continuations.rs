use chrono::{DateTime, NaiveDateTime, Utc};
use diesel::prelude::*;
use shared::{
    AgentType, ContinuationConfig, PortalMessage, ServerToClient, ServerToLauncher,
    SessionLimitContinuationFields, CONTINUATION_REASON_LIMIT, CONTINUATION_REASON_OVERLOADED,
};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::{SessionId, SessionManager};
use crate::db::DbPool;
use crate::models::{NewMessage, NewSessionContinuation, Session, SessionContinuation};
use crate::AppState;

/// Lifecycle state stored in `session_continuations.status`.
///
/// Backend-local: the column never crosses the wire as a typed value — the
/// `ServerToClient::ContinuationStatus` notification carries a broader string
/// set (it also emits a transient `"failed"` that is never persisted). The
/// wire/DB string is [`ContinuationStatus::as_str`], so stored values stay
/// byte-identical while interior branches match on the enum exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuationStatus {
    Pending,
    Scheduled,
    Fired,
    Dropped,
}

impl ContinuationStatus {
    fn as_str(self) -> &'static str {
        match self {
            ContinuationStatus::Pending => "pending",
            ContinuationStatus::Scheduled => "scheduled",
            ContinuationStatus::Fired => "fired",
            ContinuationStatus::Dropped => "dropped",
        }
    }
}

impl std::fmt::Display for ContinuationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Rolling window used to cap overload auto-retries per session. Counting prior
/// `overloaded` continuation rows in this window is the whole cap mechanism, so
/// it survives backend restarts for free (the count is DB-backed).
const OVERLOAD_RETRY_WINDOW_MINUTES: i64 = 15;
/// Maximum overload auto-retries within the window before we give up.
const OVERLOAD_RETRY_ATTEMPT_CAP: i64 = 3;

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
        status: ContinuationStatus::Pending.as_str().to_string(),
        source_message: Some(fields.source_message),
        reason: CONTINUATION_REASON_LIMIT.to_string(),
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
        continuation.reason.clone(),
    );
    let meta = insert_portal_message(&mut conn, &session, &portal).map(|m| m.portal_meta(None));

    if let Some(key) = session_key {
        session_manager.broadcast_to_web_clients(
            key,
            ServerToClient::AgentOutput {
                content: portal.to_json(),
                agent_type: session.agent_type.parse().unwrap_or_default(),
                meta,
            },
        );
    }

    info!(
        "Created session continuation {} for session {} at {}",
        continuation.id, current_session_id, continuation.reset_at
    );

    sync_continuations_for_launcher(
        &app_state.session_manager,
        &app_state.db_pool,
        launcher_id,
        session.user_id,
    );
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
            .filter(session_continuations::status.eq(ContinuationStatus::Pending.as_str())),
    )
    .set((
        session_continuations::status.eq(ContinuationStatus::Scheduled.as_str()),
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
            status: ContinuationStatus::Scheduled.as_str().to_string(),
            message: Some("Continuation scheduled".to_string()),
        },
    );
    sync_continuations_for_launcher(
        &app_state.session_manager,
        &app_state.db_pool,
        continuation.launcher_id,
        user_id,
    );
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
        ContinuationStatus::Fired,
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
        ContinuationStatus::Dropped,
        Some(reason),
    );
}

fn update_terminal_status(
    app_state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
    continuation_id: Uuid,
    session_id: Uuid,
    status: ContinuationStatus,
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
        session_continuations::status.eq(status.as_str()),
        session_continuations::last_error.eq(reason.clone()),
        session_continuations::updated_at.eq(diesel::dsl::now),
        session_continuations::fired_at.eq(if status == ContinuationStatus::Fired {
            Some(chrono::Utc::now().naive_utc())
        } else {
            None
        }),
        session_continuations::dropped_at.eq(if status == ContinuationStatus::Dropped {
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
                    status: status.as_str().to_string(),
                    message: reason.clone(),
                },
            );

            if status == ContinuationStatus::Dropped {
                if let Ok(session) = sessions::table.find(session_id).first::<Session>(&mut conn) {
                    let portal = PortalMessage::text(
                        "Continuation was not sent because the local Claude process was no longer running when the session limit reset."
                            .to_string(),
                    );
                    let meta = insert_portal_message(&mut conn, &session, &portal)
                        .map(|m| m.portal_meta(None));
                    app_state.session_manager.broadcast_to_web_clients(
                        &session_id.to_string(),
                        ServerToClient::AgentOutput {
                            content: portal.to_json(),
                            agent_type: session.agent_type.parse().unwrap_or_default(),
                            meta,
                        },
                    );
                }
            }

            sync_continuations_for_launcher(
                &app_state.session_manager,
                &app_state.db_pool,
                row.launcher_id,
                user_id,
            );
        }
        Err(e) => warn!(
            "Failed to mark continuation {} {} for session {}: {}",
            continuation_id, status, session_id, e
        ),
    }
}

pub fn sync_continuations_for_launcher(
    session_manager: &SessionManager,
    db_pool: &DbPool,
    launcher_id: Uuid,
    user_id: Uuid,
) {
    let continuations = load_scheduled_continuations(db_pool, launcher_id, user_id);
    let msg = ServerToLauncher::ContinuationSync { continuations };
    if !session_manager.send_to_launcher(&launcher_id, msg) {
        warn!(
            "Failed to sync continuations to launcher {} for user {}",
            launcher_id, user_id
        );
    }
}

pub fn load_scheduled_continuations(
    db_pool: &DbPool,
    launcher_id: Uuid,
    user_id: Uuid,
) -> Vec<ContinuationConfig> {
    let Ok(mut conn) = db_pool.get() else {
        return Vec::new();
    };

    use crate::schema::{session_continuations, sessions};
    session_continuations::table
        .inner_join(sessions::table.on(sessions::id.eq(session_continuations::session_id)))
        .filter(session_continuations::launcher_id.eq(launcher_id))
        .filter(session_continuations::user_id.eq(user_id))
        .filter(session_continuations::status.eq(ContinuationStatus::Scheduled.as_str()))
        .order(session_continuations::reset_at.asc())
        .select((SessionContinuation::as_select(), Session::as_select()))
        .load::<(SessionContinuation, Session)>(&mut conn)
        .unwrap_or_default()
        .into_iter()
        .map(|(row, session)| {
            let claude_args =
                serde_json::from_value::<Vec<String>>(session.claude_args).unwrap_or_default();
            let agent_type = session.agent_type.parse().unwrap_or_default();
            ContinuationConfig {
                id: row.id,
                session_id: row.session_id,
                reset_at: row.reset_at.to_rfc3339(),
                prompt: row.prompt,
                working_directory: Some(session.working_directory),
                session_name: Some(session.session_name),
                claude_args,
                agent_type,
                reason: row.reason,
            }
        })
        .collect()
}

/// Conservative signature for a transient provider-overload failure worth an
/// automatic retry. The CLI surfaces these as an error `Result` whose text is
/// like `"API Error: 529 Overloaded. This is a server-side issue …"`; the raw
/// provider form is `overloaded_error`. `api_error_status` is the typed status
/// from `claude_codes::io::ResultMessage` when present.
///
/// Deliberately narrow: it must NOT fire on 4xx, auth failures, or other 5xx —
/// those aren't safely auto-retryable. We require both an overload token AND a
/// 529 signal, except for the unambiguous raw `overloaded_error` string.
pub(crate) fn is_transient_overload(result_text: &str, api_error_status: Option<u16>) -> bool {
    let lower = result_text.to_ascii_lowercase();
    if lower.contains("overloaded_error") {
        return true;
    }
    let has_529 = api_error_status == Some(529) || result_text.contains("529");
    has_529 && lower.contains("overloaded")
}

/// Count `overloaded` continuations created for `session_id` within the rolling
/// retry window. This DB-backed count is the entire cap mechanism, so it holds
/// across backend restarts.
fn count_recent_overload_retries(conn: &mut diesel::PgConnection, session_id: Uuid) -> i64 {
    use crate::schema::session_continuations;
    let window_start =
        Utc::now().naive_utc() - chrono::Duration::minutes(OVERLOAD_RETRY_WINDOW_MINUTES);
    session_continuations::table
        .filter(session_continuations::session_id.eq(session_id))
        .filter(session_continuations::reason.eq(CONTINUATION_REASON_OVERLOADED))
        .filter(session_continuations::created_at.gt(window_start))
        .count()
        .get_result(conn)
        .unwrap_or(0)
}

/// Retry pacing tier from the number of prior `overloaded` retries already made
/// for this session in the window. Attempt 1 (0 prior) fires immediately — the
/// CLI already backs off internally, so the portal adds no delay of its own;
/// attempt 2 waits 60s; attempt 3 waits 300s; beyond that we give up. Returns
/// `None` once the cap is hit.
fn overload_retry_delay_secs(prior_attempts: i64) -> Option<i64> {
    match prior_attempts {
        0 => Some(0),
        1 => Some(60),
        2 => Some(300),
        _ => None,
    }
}

/// Auto-retry a turn that a transient 529 overload killed (detected at the
/// `Result` path in `message_handlers`). Reuses the usage-limit continuation
/// machinery verbatim — it inserts an already-`scheduled` `overloaded`
/// continuation (no user click), the launcher injects `prompt` into the still-
/// running session, and marks it fired — so Claude resumes the interrupted work.
///
/// `result_created_at` is the server timestamp of the failing `Result` row; a
/// newer user message means a human already stepped in and we stand down.
pub fn schedule_overloaded_retry(
    session_manager: &SessionManager,
    db_pool: &DbPool,
    session_key: &Option<SessionId>,
    session_id: Uuid,
    result_created_at: NaiveDateTime,
) {
    // Safety rail: a disconnected proxy can't receive the injected nudge, so a
    // retry would just sit scheduled. Only retry live sessions.
    if session_manager
        .current_connection_gen(&session_id.to_string())
        .is_none()
    {
        info!("Skipping overload auto-retry for {session_id}: no connected proxy");
        return;
    }

    let Ok(mut conn) = db_pool.get() else {
        error!("Failed to get DB connection for overload auto-retry");
        return;
    };

    use crate::schema::{messages, session_continuations, sessions};

    let session = match sessions::table.find(session_id).first::<Session>(&mut conn) {
        Ok(session) => session,
        Err(e) => {
            warn!("Skipping overload auto-retry for unknown session {session_id}: {e}");
            return;
        }
    };

    // Claude only — Codex overload semantics are out of scope (see spec).
    if !matches!(
        session.agent_type.parse::<AgentType>(),
        Ok(AgentType::Claude)
    ) {
        return;
    }

    let Some(launcher_id) = session.launcher_id else {
        info!("Skipping overload auto-retry for {session_id}: no launcher_id");
        return;
    };

    // Safety rail: never retry a turn a human already responded to.
    let newer_user_messages: i64 = messages::table
        .filter(messages::session_id.eq(session_id))
        .filter(messages::role.eq("user"))
        .filter(messages::created_at.gt(result_created_at))
        .count()
        .get_result(&mut conn)
        .unwrap_or(0);
    if newer_user_messages > 0 {
        info!("Skipping overload auto-retry for {session_id}: user already responded");
        return;
    }

    // Cap: prior `overloaded` retries for this session in the rolling window.
    let prior_attempts = count_recent_overload_retries(&mut conn, session_id);

    let Some(delay_secs) = overload_retry_delay_secs(prior_attempts) else {
        // Exhausted the window budget. Leave a visible marker rather than
        // inventing new push plumbing — `TurnComplete` already fired on this
        // Result, so the pocketed-phone path is covered.
        warn!(
            "Overload auto-retry gave up for session {session_id} after \
             {prior_attempts} attempt(s) in {OVERLOAD_RETRY_WINDOW_MINUTES}m"
        );
        let portal = PortalMessage::text(format!(
            "Automatic retry gave up after {OVERLOAD_RETRY_ATTEMPT_CAP} attempts following a \
             transient provider overload (HTTP 529). Please retry manually."
        ));
        let meta = insert_portal_message(&mut conn, &session, &portal).map(|m| m.portal_meta(None));
        if let Some(key) = session_key {
            session_manager.broadcast_to_web_clients(
                key,
                ServerToClient::AgentOutput {
                    content: portal.to_json(),
                    agent_type: session.agent_type.parse().unwrap_or_default(),
                    meta,
                },
            );
        }
        return;
    };

    let attempt = prior_attempts + 1;
    // `reset_at` is the intended fire time; the launcher applies no skew for
    // `overloaded` continuations, so it fires at exactly this instant.
    let reset_at = Utc::now() + chrono::Duration::seconds(delay_secs);
    let source_message = format!(
        "Turn failed with a transient provider overload (HTTP 529). \
         Auto-retrying (attempt {attempt}/{OVERLOAD_RETRY_ATTEMPT_CAP})."
    );
    let prompt = format!(
        "Automatic retry after provider overload (attempt {attempt}/{OVERLOAD_RETRY_ATTEMPT_CAP}) \
         — continue where you left off."
    );

    // Insert already-`scheduled` (no user action needed) so the existing
    // launcher fire path picks it up on the next sync.
    let new_continuation = NewSessionContinuation {
        session_id,
        user_id: session.user_id,
        launcher_id,
        reset_at,
        prompt,
        status: ContinuationStatus::Scheduled.as_str().to_string(),
        source_message: Some(source_message),
        reason: CONTINUATION_REASON_OVERLOADED.to_string(),
    };

    let continuation = match diesel::insert_into(session_continuations::table)
        .values(&new_continuation)
        .get_result::<SessionContinuation>(&mut conn)
    {
        Ok(row) => row,
        Err(e) => {
            error!("Failed to create overload continuation for {session_id}: {e}");
            return;
        }
    };

    let portal = PortalMessage::continuation_prompt(
        continuation.id,
        continuation.reset_at.to_rfc3339(),
        continuation.status.clone(),
        continuation.source_message.clone().unwrap_or_default(),
        continuation.reason.clone(),
    );
    let meta = insert_portal_message(&mut conn, &session, &portal).map(|m| m.portal_meta(None));
    if let Some(key) = session_key {
        session_manager.broadcast_to_web_clients(
            key,
            ServerToClient::AgentOutput {
                content: portal.to_json(),
                agent_type: session.agent_type.parse().unwrap_or_default(),
                meta,
            },
        );
    }

    info!(
        "Scheduled overload auto-retry {} for session {} (attempt {}/{}, +{}s)",
        continuation.id, session_id, attempt, OVERLOAD_RETRY_ATTEMPT_CAP, delay_secs
    );

    sync_continuations_for_launcher(session_manager, db_pool, launcher_id, session.user_id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{NewSessionWithId, NewUser, User};

    #[test]
    fn overload_matcher_accepts_only_transient_529_overloads() {
        // Positive: the exact user-visible CLI error text.
        assert!(is_transient_overload(
            "API Error: 529 Overloaded. This is a server-side issue, usually \
             temporary — try again in a moment. If it persists, check \
             https://status.claude.com.",
            None,
        ));
        // Positive: the raw provider form on its own.
        assert!(is_transient_overload("overloaded_error", None));
        // Positive: typed 529 status paired with an overload token.
        assert!(is_transient_overload("Overloaded", Some(529)));

        // Negative: auth, other 4xx/5xx, and generic errors.
        assert!(!is_transient_overload("401 Unauthorized", None));
        assert!(!is_transient_overload("Permission denied", None));
        assert!(!is_transient_overload("Something went wrong", Some(500)));
        // Negative: an overload word without any 529 signal is not enough.
        assert!(!is_transient_overload(
            "the server felt overloaded today",
            None
        ));
        // Negative: a 529 without an overload token stays conservative.
        assert!(!is_transient_overload("API Error: 529 gateway", Some(529)));
    }

    #[test]
    fn retry_delay_tiers() {
        assert_eq!(overload_retry_delay_secs(0), Some(0));
        assert_eq!(overload_retry_delay_secs(1), Some(60));
        assert_eq!(overload_retry_delay_secs(2), Some(300));
        assert_eq!(overload_retry_delay_secs(3), None);
        assert_eq!(overload_retry_delay_secs(4), None);
    }

    fn make_user(conn: &mut diesel::PgConnection) -> User {
        use crate::schema::users;
        let nonce = Uuid::new_v4();
        let new_user = NewUser {
            google_id: format!("test_overload_{nonce}"),
            email: format!("test_overload_{nonce}@example.invalid"),
            name: Some("Overload Test".to_string()),
            avatar_url: None,
        };
        diesel::insert_into(users::table)
            .values(&new_user)
            .get_result::<User>(conn)
            .expect("insert test user")
    }

    fn make_session(conn: &mut diesel::PgConnection, user_id: Uuid) -> Uuid {
        use crate::schema::sessions;
        let session_id = Uuid::new_v4();
        let new_session = NewSessionWithId {
            id: session_id,
            user_id,
            session_name: "overload-test".to_string(),
            session_key: session_id.to_string(),
            working_directory: "/tmp".to_string(),
            status: shared::SessionStatus::Active.as_str().to_string(),
            git_branch: None,
            client_version: None,
            hostname: "test-host".to_string(),
            launcher_id: Some(Uuid::new_v4()),
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(Vec::new()),
            launcher_version: None,
        };
        diesel::insert_into(sessions::table)
            .values(&new_session)
            .execute(conn)
            .expect("insert session");
        session_id
    }

    fn insert_overloaded_continuation(
        conn: &mut diesel::PgConnection,
        session_id: Uuid,
        user_id: Uuid,
    ) {
        use crate::schema::session_continuations;
        let row = NewSessionContinuation {
            session_id,
            user_id,
            launcher_id: Uuid::new_v4(),
            reset_at: Utc::now(),
            prompt: "retry".to_string(),
            status: ContinuationStatus::Scheduled.as_str().to_string(),
            source_message: None,
            reason: CONTINUATION_REASON_OVERLOADED.to_string(),
        };
        diesel::insert_into(session_continuations::table)
            .values(&row)
            .execute(conn)
            .expect("insert overloaded continuation");
    }

    /// 0/1/2/3 prior `overloaded` continuations in the window map to the
    /// immediate/60s/300s/give-up tiers via the real DB count.
    #[test]
    fn tier_selection_counts_recent_overload_retries() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("db conn");
        let user = make_user(&mut conn);
        let session_id = make_session(&mut conn, user.id);

        // 0 prior -> immediate.
        assert_eq!(
            overload_retry_delay_secs(count_recent_overload_retries(&mut conn, session_id)),
            Some(0)
        );
        // 1 prior -> +60s.
        insert_overloaded_continuation(&mut conn, session_id, user.id);
        assert_eq!(
            overload_retry_delay_secs(count_recent_overload_retries(&mut conn, session_id)),
            Some(60)
        );
        // 2 prior -> +300s.
        insert_overloaded_continuation(&mut conn, session_id, user.id);
        assert_eq!(
            overload_retry_delay_secs(count_recent_overload_retries(&mut conn, session_id)),
            Some(300)
        );
        // 3 prior -> give up.
        insert_overloaded_continuation(&mut conn, session_id, user.id);
        assert_eq!(
            overload_retry_delay_secs(count_recent_overload_retries(&mut conn, session_id)),
            None
        );
    }
}

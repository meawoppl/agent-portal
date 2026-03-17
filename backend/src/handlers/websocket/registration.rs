use crate::models::{NewSessionMember, NewSessionWithId};
use crate::AppState;
use diesel::prelude::*;
use shared::AgentType;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Result of a session registration attempt
pub struct RegistrationResult {
    pub success: bool,
    pub session_id: Option<Uuid>,
    pub error: Option<String>,
}

/// Parameters for registering a session
pub struct RegistrationParams<'a> {
    pub claude_session_id: Uuid,
    pub session_name: &'a str,
    pub auth_token: Option<&'a str>,
    pub working_directory: &'a str,
    pub resuming: bool,
    pub git_branch: &'a Option<String>,
    pub client_version: &'a Option<String>,
    pub session_key: &'a str,
    pub replaces_session_id: Option<Uuid>,
    pub hostname: &'a str,
    pub launcher_id: Option<Uuid>,
    pub agent_type: AgentType,
    pub repo_url: &'a Option<String>,
    pub scheduled_task_id: Option<Uuid>,
}

/// Register or update a session in the database.
/// Handles three cases: existing session reactivation, resume of unknown session, and new session creation.
pub fn register_or_update_session(
    app_state: &AppState,
    params: &RegistrationParams,
) -> RegistrationResult {
    let mut conn = match app_state.db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to get database connection for registration: {}", e);
            return RegistrationResult {
                success: false,
                session_id: None,
                error: Some("Database connection failed".to_string()),
            };
        }
    };

    use crate::schema::sessions;

    // If this session replaces a previous one, mark the old session
    if let Some(old_id) = params.replaces_session_id {
        match diesel::update(sessions::table.find(old_id))
            .set(sessions::status.eq("replaced"))
            .execute(&mut conn)
        {
            Ok(n) if n > 0 => {
                info!(
                    "Marked old session {} as replaced (superseded by {})",
                    old_id, params.claude_session_id
                );
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Failed to mark old session {} as replaced: {}", old_id, e);
            }
        }
    }

    let existing: Option<crate::models::Session> = sessions::table
        .find(params.claude_session_id)
        .first(&mut conn)
        .optional()
        .unwrap_or(None);

    if let Some(existing_session) = existing {
        match diesel::update(sessions::table.find(existing_session.id))
            .set((
                sessions::status.eq("active"),
                sessions::last_activity.eq(diesel::dsl::now),
                sessions::working_directory.eq(params.working_directory),
                sessions::git_branch.eq(params.git_branch),
                sessions::client_version.eq(params.client_version),
                sessions::hostname.eq(params.hostname),
                sessions::repo_url.eq(params.repo_url),
            ))
            .execute(&mut conn)
        {
            Ok(_) => {
                info!(
                    "Session reactivated in DB: {} ({}) branch: {:?}",
                    params.session_name, params.claude_session_id, params.git_branch
                );
                RegistrationResult {
                    success: true,
                    session_id: Some(existing_session.id),
                    error: None,
                }
            }
            Err(e) => {
                error!("Failed to reactivate session: {}", e);
                RegistrationResult {
                    success: false,
                    session_id: None,
                    error: Some("Failed to reactivate session".to_string()),
                }
            }
        }
    } else {
        if params.resuming {
            warn!(
                "Resuming session {} but not found in DB, creating new entry",
                params.claude_session_id
            );
        }

        create_new_session(app_state, &mut conn, params)
    }
}

fn create_new_session(
    app_state: &AppState,
    conn: &mut diesel::PgConnection,
    params: &RegistrationParams,
) -> RegistrationResult {
    let user_id = get_user_id_from_token(app_state, params.auth_token);
    let Some(user_id) = user_id else {
        warn!("No valid user_id for session, not persisting to DB");
        return RegistrationResult {
            success: false,
            session_id: None,
            error: Some("Authentication failed - please re-authenticate".to_string()),
        };
    };

    use crate::schema::{session_members, sessions};

    let new_session = NewSessionWithId {
        id: params.claude_session_id,
        user_id,
        session_name: params.session_name.to_string(),
        session_key: params.session_key.to_string(),
        working_directory: params.working_directory.to_string(),
        status: "active".to_string(),
        git_branch: params.git_branch.clone(),
        client_version: params.client_version.clone(),
        hostname: params.hostname.to_string(),
        launcher_id: params.launcher_id,
        agent_type: params.agent_type.as_str().to_string(),
        repo_url: params.repo_url.clone(),
        scheduled_task_id: params.scheduled_task_id,
    };

    match diesel::insert_into(sessions::table)
        .values(&new_session)
        .get_result::<crate::models::Session>(conn)
    {
        Ok(session) => {
            let new_member = NewSessionMember {
                session_id: session.id,
                user_id,
                role: "owner".to_string(),
            };
            if let Err(e) = diesel::insert_into(session_members::table)
                .values(&new_member)
                .execute(conn)
            {
                error!("Failed to create session_member: {}", e);
            }

            info!(
                "Session persisted to DB: {} ({}) branch: {:?} agent: {}",
                params.session_name, params.claude_session_id, params.git_branch, params.agent_type
            );
            RegistrationResult {
                success: true,
                session_id: Some(session.id),
                error: None,
            }
        }
        Err(e) => {
            error!("Failed to persist session: {}", e);
            RegistrationResult {
                success: false,
                session_id: None,
                error: Some("Failed to persist session".to_string()),
            }
        }
    }
}

/// Get user_id from auth token using JWT verification
fn get_user_id_from_token(app_state: &AppState, auth_token: Option<&str>) -> Option<Uuid> {
    let mut conn = app_state.db_pool.get().ok()?;
    use crate::schema::users;

    if let Some(token) = auth_token {
        match super::super::proxy_tokens::verify_and_get_user(app_state, &mut conn, token) {
            Ok((user_id, email)) => {
                info!("JWT token verified for user: {}", email);
                return Some(user_id);
            }
            Err(e) => {
                warn!("JWT verification failed: {:?}, falling back to dev mode", e);
            }
        }
    }

    if app_state.dev_mode {
        users::table
            .filter(users::email.eq("testing@testing.local"))
            .select(users::id)
            .first::<Uuid>(&mut conn)
            .ok()
    } else {
        None
    }
}

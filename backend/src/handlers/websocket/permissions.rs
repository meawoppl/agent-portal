use super::{SessionId, SessionManager, WebClientSender};
use crate::db::DbPool;
use diesel::prelude::*;
use shared::{ServerToClient, ServerToProxy};
use tracing::{error, info, warn};
use uuid::Uuid;

/// Store a permission request in the database and forward it to web clients.
#[allow(clippy::too_many_arguments)]
pub fn handle_permission_request(
    session_manager: &SessionManager,
    session_key: &Option<String>,
    db_session_id: Option<Uuid>,
    db_pool: &DbPool,
    request_id: String,
    tool_name: String,
    input: serde_json::Value,
    permission_suggestions: Vec<shared::PermissionSuggestion>,
) {
    // Store in database for replay on reconnect
    if let Some(session_id) = db_session_id {
        match db_pool.get() {
            Ok(mut conn) => {
                use crate::schema::pending_permission_requests;

                let suggestions_json = if permission_suggestions.is_empty() {
                    None
                } else {
                    Some(serde_json::to_value(&permission_suggestions).unwrap_or_default())
                };

                let new_request = crate::models::NewPendingPermissionRequest {
                    session_id,
                    request_id: request_id.clone(),
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                    permission_suggestions: suggestions_json.clone(),
                };

                if let Err(e) = diesel::insert_into(pending_permission_requests::table)
                    .values(&new_request)
                    .on_conflict(pending_permission_requests::session_id)
                    .do_update()
                    .set((
                        pending_permission_requests::request_id.eq(&request_id),
                        pending_permission_requests::tool_name.eq(&tool_name),
                        pending_permission_requests::input.eq(&input),
                        pending_permission_requests::permission_suggestions.eq(suggestions_json),
                        pending_permission_requests::created_at.eq(diesel::dsl::now),
                    ))
                    .execute(&mut conn)
                {
                    error!("Failed to store pending permission request: {}", e);
                }
            }
            Err(e) => {
                error!(
                    "Failed to get database connection for storing permission request: {}",
                    e
                );
            }
        }
    }

    // Forward to web clients
    if let Some(ref key) = session_key {
        info!(
            "Permission request from proxy for tool: {} (request_id: {}, suggestions: {})",
            tool_name,
            request_id,
            permission_suggestions.len()
        );
        session_manager.broadcast_to_web_clients(
            key,
            ServerToClient::PermissionRequest {
                request_id,
                tool_name,
                input,
                permission_suggestions,
            },
        );
    }
}

/// Handle a permission response from a web client: clear from DB and forward to proxy.
#[allow(clippy::too_many_arguments)]
pub fn handle_permission_response(
    session_manager: &SessionManager,
    session_key: &SessionId,
    session_id: Uuid,
    db_pool: &DbPool,
    request_id: String,
    allow: bool,
    input: Option<serde_json::Value>,
    permissions: Vec<shared::PermissionSuggestion>,
    reason: Option<String>,
) {
    info!(
        "Web client sending PermissionResponse: {} -> {} (permissions: {}, reason: {:?})",
        request_id,
        if allow { "allow" } else { "deny" },
        permissions.len(),
        reason
    );

    // Clear pending permission request from database
    match db_pool.get() {
        Ok(mut conn) => {
            use crate::schema::pending_permission_requests;
            if let Err(e) = diesel::delete(
                pending_permission_requests::table
                    .filter(pending_permission_requests::session_id.eq(session_id)),
            )
            .execute(&mut conn)
            {
                error!("Failed to clear pending permission request: {}", e);
            }
        }
        Err(e) => {
            error!(
                "Failed to get database connection for clearing permission request: {}",
                e
            );
        }
    }

    if !session_manager.send_to_session(
        session_key,
        ServerToProxy::PermissionResponse(shared::PermissionResponseFields {
            request_id,
            allow,
            input,
            permissions,
            reason,
        }),
    ) {
        warn!(
            "Failed to send PermissionResponse to session '{}', session not connected",
            session_key
        );
    }
}

/// Replay a pending permission request from the database to a newly connected web client.
pub fn replay_pending_permission(db_pool: &DbPool, session_id: Uuid, tx: &WebClientSender) {
    let mut conn = match db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            warn!(
                "Failed to get database connection for replaying permission request: {}",
                e
            );
            return;
        }
    };

    use crate::schema::pending_permission_requests;
    if let Ok(pending) = pending_permission_requests::table
        .filter(pending_permission_requests::session_id.eq(session_id))
        .first::<crate::models::PendingPermissionRequest>(&mut conn)
    {
        info!(
            "Replaying pending permission request for session {}: {} ({})",
            session_id, pending.tool_name, pending.request_id
        );

        let suggestions: Vec<shared::PermissionSuggestion> = pending
            .permission_suggestions
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        let _ = tx.send(ServerToClient::PermissionRequest {
            request_id: pending.request_id,
            tool_name: pending.tool_name,
            input: pending.input,
            permission_suggestions: suggestions,
        });
    }
}

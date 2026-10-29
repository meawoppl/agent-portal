use super::permissions::{handle_permission_response, replay_pending_permission};
use super::replay::replay_history;
use super::session_manager::{DedupVerdict, EnqueueInput, InputDeliveryState};
use super::uploads::{handle_file_upload_chunk, handle_file_upload_start, PendingUpload};
use super::{SessionId, SessionManager, WebClientSender};
use crate::handlers::session_access::is_session_mutator;
use crate::AppState;
use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{
    ClientEndpoint, ClientToServer, PortalMessage, SendMode, ServerToClient, ServerToProxy,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

const LINKED_RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Only outbound reads may reuse successful authorization. Incoming commands
/// always validate freshly, and the idle timer forces validation even under load.
#[derive(Default)]
struct OutboundAuthorization {
    last_success: Option<std::time::Instant>,
}

impl OutboundAuthorization {
    fn check<E>(
        &mut self,
        now: std::time::Instant,
        force: bool,
        validate: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        if !force
            && self
                .last_success
                .is_some_and(|last| now.duration_since(last) < LINKED_RECHECK_INTERVAL)
        {
            return Ok(());
        }
        // Never retain a success across a failed forced check.
        self.last_success = None;
        validate()?;
        self.last_success = Some(now);
        Ok(())
    }
}

#[tracing::instrument(skip_all, fields(user, user_id = %user_id, session_name, session_id, computer, owner, owner_id))]
pub async fn handle_web_client_socket(
    socket: WebSocket,
    app_state: Arc<AppState>,
    user_id: Uuid,
    linked: Option<crate::handlers::federation::LinkedAuth>,
) {
    if let Ok(mut conn) = app_state.db_pool.get() {
        use crate::schema::users;
        if let Ok(user) = users::table
            .find(user_id)
            .first::<crate::models::User>(&mut conn)
        {
            crate::log_context::record_user(&user);
        }
    }
    let session_manager = app_state.session_manager.clone();
    let db_pool = app_state.db_pool.clone();
    let initial_replay_limit = app_state.message_retention_count;
    let conn = ws_bridge::server::into_connection::<ClientEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();
    let (tx, mut rx) =
        super::session_manager::conn_channel::<ServerToClient>(super::WEB_CLIENT_CHANNEL_CAPACITY);

    let mut dashboard_jobs = tokio::task::JoinSet::new();
    let mut dashboard_ids = std::collections::HashSet::new();
    let mut compatibility_checked = false;
    let mut session_key: Option<SessionId> = None;
    let mut verified_session_id: Option<Uuid> = None;
    let mut pending_uploads: HashMap<String, PendingUpload> = HashMap::new();

    session_manager.add_user_client_with_progress(user_id, tx.clone());
    // Seed the services monitor so the header shows figures before the
    // first periodic broadcast lands.
    let _ = tx.send(ServerToClient::ServiceStatsUpdate {
        services: crate::service_stats::table_for_user(&app_state, user_id),
    });

    let ctx = WebClientCtx {
        app_state: &app_state,
        session_manager: &session_manager,
        db_pool: &db_pool,
        tx: &tx,
        user_id,
        initial_replay_limit,
    };

    // Streaming output shares a two-second authorization window per socket;
    // every incoming command still checks live authority. Forced idle checks
    // close revoked sockets even when no traffic is flowing.
    let mut outbound_auth = OutboundAuthorization::default();
    let mut recheck = tokio::time::interval(LINKED_RECHECK_INTERVAL);
    loop {
        tokio::select! {
            completed = dashboard_jobs.join_next(), if !dashboard_jobs.is_empty() => {
                if let Some(Ok((request_id, result))) = completed {
                    dashboard_ids.remove(&request_id);
                    if tx.send(ServerToClient::DashboardResult { request_id, result }).is_err() { break; }
                } else {
                    // A failed task has no trustworthy correlated reply. Closing
                    // fails all browser waiters immediately; none are replayed.
                    warn!("Dashboard task failed; closing client socket");
                    break;
                }
            }
            _ = recheck.tick(), if linked.is_some() => {
                if linked.as_ref().is_some_and(|auth| outbound_auth.check(std::time::Instant::now(), true, || auth.authorize(&app_state, shared::api::TokenScope::Read)).is_err()) { break; }
            }
            msg = rx.recv() => {
                let Some(msg) = msg else { break; };
                if linked.as_ref().is_some_and(|auth| outbound_auth.check(std::time::Instant::now(), false, || auth.authorize(&app_state, shared::api::TokenScope::Read)).is_err()) { break; }
                if ws_sender.send(msg).await.is_err() { break; }
            }
            result = ws_receiver.recv() => {
                let Some(result) = result else { break; };
                match result {
                    Ok(client_msg) => {
                        if linked.as_ref().is_some_and(|auth| auth.authorize(&app_state, crate::handlers::federation::message_scope(&client_msg)).is_err()) { break; }
                        // Negotiate inline: a rejection must reach the socket before
                        // closing, and no later pipelined mutation may dispatch.
                        let version = match &client_msg {
                            ClientToServer::Hello { protocol_version } => Some(Some(*protocol_version)),
                            ClientToServer::Register(fields) => Some(fields.protocol_version),
                            // An old dashboard can start without Register or Hello.
                            // Treat that as v1, never as whichever major is current.
                            _ if !compatibility_checked => Some(None),
                            _ => None,
                        };
                        if let Some(version) = version {
                            let error = shared::protocol::check_protocol_version(version, "browser").err();
                            compatibility_checked = error.is_none();
                            if matches!(client_msg, ClientToServer::Hello { .. }) {
                                let rejected = error.is_some();
                                if ws_sender.send(ServerToClient::HelloAck {
                                    protocol_version: shared::protocol::PROTOCOL_VERSION,
                                    success: !rejected,
                                    error,
                                }).await.is_err() || rejected { break; }
                                continue;
                            }
                            if let Some(message) = error {
                                // Legacy browsers know Error, not HelloAck. Keep the
                                // rejection decodable even across a future major bump.
                                let _ = ws_sender.send(ServerToClient::Error { message }).await;
                                break;
                            }
                        }
                        if let ClientToServer::Dashboard { request_id, request } = client_msg {
                            // Linked grants currently exclude host administration on REST;
                            // a socket must not bypass that narrower allow-list.
                            let error = if linked.is_some() {
                                Some((403, "Host management requires the home account"))
                            } else if dashboard_ids.contains(&request_id) {
                                Some((409, "Request id is already in flight"))
                            } else if dashboard_jobs.len() >= 32 {
                                Some((429, "Too many pending dashboard requests"))
                            } else { None };
                            if let Some((status, message)) = error {
                                if tx.send(ServerToClient::DashboardResult { request_id, result: Err(shared::api::DashboardError { status, message: message.into() }) }).is_err() { break; }
                            } else {
                                dashboard_ids.insert(request_id);
                                let state = app_state.clone();
                                dashboard_jobs.spawn(async move {
                                    let result = crate::handlers::dashboard_rpc::execute(state, user_id, request).await;
                                    (request_id, result)
                                });
                            }
                            continue;
                        }
                        if handle_web_client_message(client_msg, ctx, &mut session_key, &mut verified_session_id, &mut pending_uploads) { break; }
                    }
                    Err(e) => { warn!("WebSocket decode error: {}", e); }
                }
            }
        }
    }

    // Eagerly drop this connection's senders from the presence registries.
    // All exit paths of the loop above (client close, decode-fatal break,
    // access-denied replace) funnel through here, so presence never retains
    // a dead sender — push-notification suppression depends on that
    // (docs/MOBILE_APPS_PLAN.md §8.2). The lazy prune in `fanout_to_clients`
    // remains as a backstop for the send-failure path.
    session_manager.remove_user_client(&user_id, &tx);
    if let Some(ref key) = session_key {
        session_manager.remove_web_client(key, &tx);
    }
}

/// Shared per-connection context for the web-client message handlers below.
/// Bundled so the handler signatures stay under the argument-count lint;
/// `Copy` so dispatch arms can pass it down freely. The three `&mut`
/// connection states stay as separate params — they are per-message
/// mutable borrows, not shared context.
#[derive(Clone, Copy)]
struct WebClientCtx<'a> {
    app_state: &'a AppState,
    session_manager: &'a SessionManager,
    db_pool: &'a crate::db::DbPool,
    tx: &'a WebClientSender,
    user_id: Uuid,
    initial_replay_limit: i64,
}

/// Refuse a viewer's mutation with a reply the browser shows. A silent drop
/// leaves its UI waiting on an answer that never comes (a prompt held for its
/// uploads, a permission dialog that closed as if answered).
fn reject_viewer(tx: &WebClientSender, user_id: Uuid, session_id: Uuid, action: &str) {
    warn!("Viewer-role user {user_id} attempted to {action} on session {session_id}");
    let _ = tx.send(ServerToClient::Error {
        message: format!("You don't have permission to {action} in this session"),
    });
}

/// Returns true if the connection should be closed
fn handle_web_client_message(
    client_msg: ClientToServer,
    ctx: WebClientCtx<'_>,
    session_key: &mut Option<SessionId>,
    verified_session_id: &mut Option<Uuid>,
    pending_uploads: &mut HashMap<String, PendingUpload>,
) -> bool {
    let WebClientCtx {
        app_state,
        session_manager,
        db_pool,
        tx,
        user_id,
        ..
    } = ctx;
    match client_msg {
        ClientToServer::Dashboard { .. } => false, // handled asynchronously by the socket
        // Consumed by the socket handshake before command dispatch.
        ClientToServer::Hello { .. } => false,
        ClientToServer::Register(shared::RegisterFields {
            session_id,
            session_name,
            replay_after,
            ..
        }) => handle_web_register(
            ctx,
            session_id,
            &session_name,
            replay_after,
            session_key,
            verified_session_id,
        ),
        ClientToServer::AgentInput {
            content,
            send_mode,
            reasoning_effort,
            client_msg_id,
        } => {
            // Gate on editor/owner role — re-queried on each input so role
            // revocations take effect immediately for already-connected viewers.
            // See `session_access::is_session_mutator` for the layered rules.
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    if let Some(id) = client_msg_id {
                        let _ = tx.send(ServerToClient::InputProgress {
                            client_msg_id: id,
                            stage: shared::InputDeliveryStage::Failed,
                            message: Some("permission denied".to_string()),
                            queued: false,
                        });
                    }
                    let _ = tx.send(ServerToClient::Error {
                        message: "You don't have permission to send input to this session"
                            .to_string(),
                    });
                    return false;
                }
            }
            // Idempotency gate (#1236): the outbox resends everything unacked
            // after a reconnect, so a resent id may already be tracked. Known
            // terminal → re-emit the terminal stage (idempotent ack) and stop;
            // known in-flight → drop the duplicate (the original's acks will
            // arrive); new → recorded in-flight, proceed.
            if let (Some(id), Some(session_id)) = (client_msg_id, *verified_session_id) {
                let verdict = match session_manager.check_and_record_input(session_id, id) {
                    // The in-memory tracker is empty after a backend restart;
                    // a pending_inputs row with this id proves the original
                    // was already accepted for delivery.
                    DedupVerdict::New if pending_input_exists(db_pool, session_id, id) => {
                        DedupVerdict::Duplicate(InputDeliveryState::InFlight)
                    }
                    v => v,
                };
                match verdict {
                    DedupVerdict::New => {}
                    DedupVerdict::Duplicate(InputDeliveryState::InFlight) => return false,
                    DedupVerdict::Duplicate(InputDeliveryState::Accepted) => {
                        let _ = tx.send(ServerToClient::InputProgress {
                            client_msg_id: id,
                            stage: shared::InputDeliveryStage::AgentAccepted,
                            message: None,
                            queued: false,
                        });
                        return false;
                    }
                    DedupVerdict::Duplicate(InputDeliveryState::Failed) => {
                        let _ = tx.send(ServerToClient::InputProgress {
                            client_msg_id: id,
                            stage: shared::InputDeliveryStage::Failed,
                            message: Some("delivery previously failed".to_string()),
                            queued: false,
                        });
                        return false;
                    }
                }
            }
            // First delivery stage: the backend accepted the input frame. Later
            // stages (ProxyReceived / AgentAccepted) come from the proxy ack
            // path.
            if let Some(id) = client_msg_id {
                let _ = tx.send(ServerToClient::InputProgress {
                    client_msg_id: id,
                    stage: shared::InputDeliveryStage::ServerReceived,
                    message: None,
                    queued: false,
                });
            }
            handle_web_input(
                ctx,
                session_key,
                *verified_session_id,
                WebInput {
                    content,
                    send_mode,
                    reasoning_effort,
                    client_msg_id,
                },
                user_id,
            );
            false
        }
        ClientToServer::FileUploadStart(shared::FileUploadStartFields {
            upload_id,
            filename,
            content_type,
            total_chunks,
            total_size,
            disposition,
        }) => {
            // File uploads end up as `ClaudeInput` on the proxy side — they're
            // mutations too, gate them on editor/owner role.
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    warn!("Viewer-role user {user_id} attempted FileUploadStart on session {session_id}");
                    super::uploads::send_upload_failure(tx, upload_id, "permission denied");
                    return false;
                }
            }
            handle_file_upload_start(
                session_manager,
                session_key,
                tx,
                pending_uploads,
                upload_id,
                filename,
                content_type,
                total_chunks,
                total_size,
                app_state.max_image_mb,
                disposition,
            );
            false
        }
        ClientToServer::FileUploadChunk(shared::FileUploadChunkFields {
            upload_id,
            chunk_index,
            data,
        }) => {
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    warn!("Viewer-role user {user_id} attempted FileUploadChunk on session {session_id}");
                    // Demoted mid-upload: drop what was received, and answer
                    // once, so the browser stops waiting on it.
                    if pending_uploads.remove(&upload_id).is_some() {
                        super::uploads::send_upload_failure(tx, upload_id, "permission denied");
                    }
                    return false;
                }
            }
            handle_file_upload_chunk(
                session_manager,
                session_key,
                tx,
                pending_uploads,
                upload_id,
                chunk_index,
                data,
            );
            false
        }
        ClientToServer::PermissionResponse(shared::PermissionResponseFields {
            request_id,
            allow,
            input,
            permissions,
            reason,
        }) => {
            if let (Some(ref key), Some(session_id)) = (session_key, *verified_session_id) {
                // Approving/denying tool permissions decides whether the
                // proxy executes the tool — that's a mutation, gate it.
                if !is_session_mutator(app_state, session_id, user_id) {
                    reject_viewer(tx, user_id, session_id, "answer permission requests");
                    return false;
                }
                handle_permission_response(
                    session_manager,
                    key,
                    session_id,
                    db_pool,
                    request_id,
                    allow,
                    input,
                    permissions,
                    reason,
                );
            } else {
                warn!("Web client tried to send PermissionResponse without verified session");
            }
            false
        }
        ClientToServer::Interrupt => {
            if let Some(ref key) = session_key {
                // Interrupt cancels the in-flight model turn — a mutation.
                if let Some(session_id) = *verified_session_id {
                    if !is_session_mutator(app_state, session_id, user_id) {
                        reject_viewer(tx, user_id, session_id, "stop the agent");
                        return false;
                    }
                }
                info!("Web client sending interrupt to session");
                session_manager.send_to_session(key, ServerToProxy::Interrupt);
            } else {
                warn!("Web client tried to send Interrupt without registered session");
            }
            false
        }
        ClientToServer::ScheduleLimitContinuation { continuation_id } => {
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    // Settles this tab's "Scheduling..." button; the
                    // continuation itself is untouched for everyone else.
                    let _ = tx.send(ServerToClient::ContinuationStatus {
                        continuation_id,
                        status: "failed".to_string(),
                        message: Some("permission denied".to_string()),
                    });
                    reject_viewer(tx, user_id, session_id, "schedule a continuation");
                    return false;
                }
                super::continuations::schedule_limit_continuation(
                    app_state,
                    session_manager,
                    user_id,
                    session_id,
                    continuation_id,
                );
            } else {
                warn!("Web client tried to schedule continuation without verified session");
            }
            false
        }
    }
}

/// Handle web client registration. Returns true if the connection should be closed.
fn handle_web_register(
    ctx: WebClientCtx<'_>,
    session_id: Uuid,
    session_name: &str,
    replay_after: Option<String>,
    session_key: &mut Option<SessionId>,
    verified_session_id: &mut Option<Uuid>,
) -> bool {
    let WebClientCtx {
        app_state,
        session_manager,
        db_pool,
        tx,
        user_id,
        initial_replay_limit,
    } = ctx;
    let access = app_state.conn().and_then(|mut conn| {
        crate::handlers::session_access::verify_session_reader(&mut conn, session_id, user_id)
    });
    match access {
        Ok(_session) => {
            if let Ok(mut conn) = db_pool.get() {
                crate::log_context::record_session(&mut conn, session_id);
            }
            let key = SessionId::new(session_id.to_string());
            *session_key = Some(key.clone());
            *verified_session_id = Some(session_id);

            session_manager.add_web_client_with_progress(session_id, tx.clone());
            info!(
                "Web client connected to session: {} ({}) for user {}",
                session_name, session_id, user_id
            );

            replay_history(db_pool, tx, session_id, replay_after, initial_replay_limit);
            replay_pending_permission(db_pool, session_id, tx);
            false
        }
        Err(e) => {
            warn!(
                "User {} attempted to access session {} they don't own: {:?}",
                user_id, session_id, e
            );
            let _ = tx.send(ServerToClient::Error {
                message: "Access denied: you don't own this session".to_string(),
            });
            true // close connection
        }
    }
}

/// DB backstop for the idempotency gate (#1236): the in-memory tracker dies
/// with the backend, but an undelivered original leaves a `pending_inputs`
/// row carrying its `client_msg_id`. Errors read as "not found" — a dup
/// slipping through on a DB blip is the at-least-once side of the contract.
fn pending_input_exists(
    db_pool: &crate::db::DbPool,
    session_id: Uuid,
    client_msg_id: Uuid,
) -> bool {
    use crate::schema::pending_inputs;
    let Ok(mut conn) = db_pool.get() else {
        return false;
    };
    diesel::select(diesel::dsl::exists(
        pending_inputs::table
            .filter(pending_inputs::session_id.eq(session_id))
            .filter(pending_inputs::client_msg_id.eq(client_msg_id)),
    ))
    .get_result::<bool>(&mut conn)
    .unwrap_or(false)
}

struct WebInput {
    content: serde_json::Value,
    send_mode: Option<SendMode>,
    reasoning_effort: Option<shared::ReasoningEffort>,
    client_msg_id: Option<Uuid>,
}

fn handle_web_input(
    ctx: WebClientCtx<'_>,
    session_key: &Option<SessionId>,
    verified_session_id: Option<Uuid>,
    input: WebInput,
    user_id: Uuid,
) {
    let WebClientCtx {
        session_manager,
        db_pool,
        ..
    } = ctx;
    let Some(ref key) = session_key else {
        warn!("Web client tried to send ClaudeInput but no session_key set (not registered?)");
        return;
    };
    let Some(session_id) = verified_session_id else {
        warn!("Attempted ClaudeInput without verified session ownership");
        return;
    };

    info!("Web client sending ClaudeInput to session: {}", key);

    // Track who sent this input so we can attribute the echoed user message
    if let Ok(mut conn) = db_pool.get() {
        let display_name: String = crate::handlers::helpers::user_display_name(&mut conn, user_id)
            .unwrap_or_else(|| "Unknown".to_string());
        session_manager.set_last_input_sender(session_id, user_id, display_name);
    }

    // For slash commands, broadcast a portal message so the user sees feedback
    if let serde_json::Value::String(text) = &input.content {
        if text.starts_with('/') {
            let portal = PortalMessage::text(format!("`{}`", text));

            // Look up session once so the broadcast and DB insert share the
            // same agent_type (the session row is the source of truth here —
            // these messages don't originate from a proxy emission).
            let session = db_pool.get().ok().and_then(|mut conn| {
                use crate::schema::sessions;
                sessions::table
                    .find(session_id)
                    .first::<crate::models::Session>(&mut conn)
                    .ok()
            });

            let agent_type = session
                .as_ref()
                .and_then(|s| s.agent_type.parse::<shared::AgentType>().ok())
                .unwrap_or_default();

            // Insert first so the broadcast's `meta` carries the server-assigned
            // `created_at` (closes #784 — same fix as handle_agent_output).
            // Typed portal sidecar for the broadcast (source = Portal), so the
            // meta-only frontend gets created_at + source (#portal-meta).
            let mut broadcast_meta: Option<shared::PortalMeta> = None;
            if let (Some(session), Ok(mut conn)) = (session.as_ref(), db_pool.get()) {
                use crate::schema::messages;
                let new_message = crate::models::NewMessage {
                    session_id,
                    role: "portal".to_string(),
                    content: serde_json::to_string(&portal.to_json()).unwrap_or_default(),
                    user_id,
                    agent_type: session.agent_type.clone(),
                    provenance_kind: None,
                    provenance_session_id: None,
                    provenance_agent_type: None,
                };
                match diesel::insert_into(messages::table)
                    .values(&new_message)
                    .get_result::<crate::models::Message>(&mut conn)
                {
                    Ok(inserted) => {
                        broadcast_meta = Some(inserted.portal_meta(None));
                    }
                    Err(e) => {
                        error!("Failed to store portal message: {}", e);
                    }
                }
            }

            session_manager.broadcast_to_web_clients(
                key,
                ServerToClient::AgentOutput {
                    content: portal.to_json(),
                    agent_type,
                    meta: broadcast_meta,
                },
            );
        }
    }

    // Seq bump + best-effort persist + live delivery, shared with the
    // agent-messaging send path (see SessionManager::enqueue_input).
    let client_msg_id = input.client_msg_id;
    let outcome = session_manager.enqueue_input(
        db_pool,
        key,
        session_id,
        EnqueueInput {
            content: input.content,
            send_mode: input.send_mode,
            reasoning_effort: input.reasoning_effort,
            client_msg_id,
        },
    );
    if !outcome.live {
        info!("Agent for session '{key}' is offline; input held for its reconnect");
        // Tell only the sender it is waiting for the agent, and only when it
        // will really replay. Nothing is resent from here; the proxy's acks
        // after it reconnects settle the row as usual.
        if let (Some(id), true) = (client_msg_id, outcome.durable) {
            let _ = ctx.tx.send(ServerToClient::InputProgress {
                client_msg_id: id,
                stage: shared::InputDeliveryStage::ServerReceived,
                message: None,
                queued: true,
            });
        }
    }
}

#[cfg(test)]
mod outbound_authorization_tests {
    use super::*;

    #[test]
    fn streaming_reuses_success_but_expiry_and_idle_force_validation() {
        let mut cache = OutboundAuthorization::default();
        let start = std::time::Instant::now();
        let calls = std::cell::Cell::new(0);
        let validate = || {
            calls.set(calls.get() + 1);
            Ok::<_, ()>(())
        };
        for _ in 0..1000 {
            cache.check(start, false, validate).unwrap();
        }
        assert_eq!(calls.get(), 1);
        cache
            .check(start + LINKED_RECHECK_INTERVAL, false, validate)
            .unwrap();
        assert_eq!(calls.get(), 2);
        cache
            .check(start + LINKED_RECHECK_INTERVAL, true, validate)
            .unwrap();
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn failed_forced_check_invalidates_previous_success() {
        let mut cache = OutboundAuthorization::default();
        let now = std::time::Instant::now();
        cache.check(now, false, || Ok::<_, ()>(())).unwrap();
        assert!(cache.check(now, true, || Err(())).is_err());
        assert!(cache.check(now, false, || Err(())).is_err());
    }
}

#[cfg(test)]
mod viewer_rejection_tests {
    use super::*;
    use crate::models::NewSessionMember;
    use crate::schema::session_members;
    use crate::test_support::{insert_session, insert_user, shared_pool, test_app_state};

    fn send(
        state: &AppState,
        tx: &WebClientSender,
        user_id: Uuid,
        session: Uuid,
        uploads: &mut HashMap<String, PendingUpload>,
        message: ClientToServer,
    ) {
        let ctx = WebClientCtx {
            app_state: state,
            session_manager: &state.session_manager,
            db_pool: &state.db_pool,
            tx,
            user_id,
            initial_replay_limit: 0,
        };
        handle_web_client_message(
            message,
            ctx,
            &mut Some(SessionId::from(session.to_string())),
            &mut Some(session),
            uploads,
        );
    }

    fn start(upload_id: &str) -> ClientToServer {
        ClientToServer::FileUploadStart(shared::FileUploadStartFields {
            upload_id: upload_id.into(),
            filename: "notes.txt".into(),
            content_type: "text/plain".into(),
            total_chunks: 2,
            total_size: 10,
            disposition: shared::FileUploadDisposition::Workspace,
        })
    }

    fn chunk(upload_id: &str) -> ClientToServer {
        ClientToServer::FileUploadChunk(shared::FileUploadChunkFields {
            upload_id: upload_id.into(),
            chunk_index: 0,
            data: "aGVsbG8=".into(),
        })
    }

    fn upload_failure(message: ServerToClient) -> (String, Option<String>) {
        match message {
            ServerToClient::FileUploadResult(result) => {
                assert!(!result.success);
                (result.upload_id, result.error)
            }
            other => panic!("expected an upload failure, got {other:?}"),
        }
    }

    fn refusal(message: ServerToClient) -> String {
        match message {
            ServerToClient::Error { message } => message,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    /// A member demoted to viewer gets an answer for every refused mutation,
    /// on their own socket only, and a refused chunk drops only their own
    /// upload.
    #[test]
    fn demoted_member_gets_a_reply_for_every_refused_mutation() {
        let Some(pool) = shared_pool() else { return };
        let state = test_app_state(pool);
        let mut conn = state.conn().unwrap();
        let owner = insert_user(&mut conn, "viewer-reject-owner");
        let editor = insert_user(&mut conn, "viewer-reject-editor");
        let session = insert_session(&mut conn, owner.id, "viewer-reject");
        diesel::insert_into(session_members::table)
            .values(NewSessionMember {
                session_id: session.id,
                user_id: editor.id,
                role: "editor".into(),
            })
            .execute(&mut conn)
            .unwrap();
        let (proxy_tx, mut proxy_rx) = super::super::conn_channel(64);
        state.session_manager.register_session(
            SessionId::from(session.id.to_string()),
            proxy_tx,
            tokio_util::sync::CancellationToken::new(),
        );
        let (tx, mut rx) = super::super::conn_channel(64);
        let (owner_tx, mut owner_rx) = super::super::conn_channel(64);
        state
            .session_manager
            .add_web_client(SessionId::from(session.id.to_string()), owner_tx);
        let mut uploads = HashMap::new();
        // Another member's connection, holding an upload with the same id.
        let mut owner_uploads = HashMap::new();
        send(
            &state,
            &tx,
            editor.id,
            session.id,
            &mut uploads,
            start("u1"),
        );
        send(
            &state,
            &tx,
            owner.id,
            session.id,
            &mut owner_uploads,
            start("u1"),
        );
        assert!(uploads.contains_key("u1") && owner_uploads.contains_key("u1"));
        while proxy_rx.try_recv().is_ok() {}

        diesel::update(session_members::table)
            .filter(session_members::session_id.eq(session.id))
            .filter(session_members::user_id.eq(editor.id))
            .set(session_members::role.eq("viewer"))
            .execute(&mut conn)
            .unwrap();

        // Mid-upload: the partial upload is dropped and answered once.
        send(
            &state,
            &tx,
            editor.id,
            session.id,
            &mut uploads,
            chunk("u1"),
        );
        assert_eq!(
            upload_failure(rx.try_recv().unwrap()),
            ("u1".into(), Some("permission denied".into()))
        );
        assert!(uploads.is_empty());
        assert!(owner_uploads.contains_key("u1"));
        send(
            &state,
            &tx,
            editor.id,
            session.id,
            &mut uploads,
            chunk("u1"),
        );
        // A guessed id names nothing on this connection: nothing to answer.
        send(
            &state,
            &tx,
            editor.id,
            session.id,
            &mut uploads,
            chunk("someone-else"),
        );
        assert!(rx.try_recv().is_err());

        send(
            &state,
            &tx,
            editor.id,
            session.id,
            &mut uploads,
            start("u2"),
        );
        assert_eq!(upload_failure(rx.try_recv().unwrap()).0, "u2");

        let answer = ClientToServer::PermissionResponse(shared::PermissionResponseFields {
            request_id: "r1".into(),
            allow: true,
            input: None,
            permissions: Vec::new(),
            reason: None,
        });
        send(&state, &tx, editor.id, session.id, &mut uploads, answer);
        assert!(refusal(rx.try_recv().unwrap()).contains("answer permission requests"));

        send(
            &state,
            &tx,
            editor.id,
            session.id,
            &mut uploads,
            ClientToServer::Interrupt,
        );
        assert!(refusal(rx.try_recv().unwrap()).contains("stop the agent"));

        let continuation_id = Uuid::new_v4();
        send(
            &state,
            &tx,
            editor.id,
            session.id,
            &mut uploads,
            ClientToServer::ScheduleLimitContinuation { continuation_id },
        );
        match rx.try_recv().unwrap() {
            ServerToClient::ContinuationStatus {
                continuation_id: id,
                status,
                ..
            } => assert_eq!((id, status.as_str()), (continuation_id, "failed")),
            other => panic!("expected a continuation status, got {other:?}"),
        }
        assert!(refusal(rx.try_recv().unwrap()).contains("schedule a continuation"));

        // Nothing reached the agent, and no other member heard about it.
        assert!(proxy_rx.try_recv().is_err());
        assert!(owner_rx.try_recv().is_err());
    }
}

#[cfg(test)]
mod queued_input_tests {
    use super::*;
    use crate::models::NewPendingInput;
    use crate::schema::{pending_inputs, sessions};
    use crate::test_support::{insert_session, insert_user, shared_pool, test_app_state};

    fn send_input(
        state: &AppState,
        tx: &WebClientSender,
        user_id: Uuid,
        session: Uuid,
        client_msg_id: Uuid,
    ) {
        let ctx = WebClientCtx {
            app_state: state,
            session_manager: &state.session_manager,
            db_pool: &state.db_pool,
            tx,
            user_id,
            initial_replay_limit: 0,
        };
        handle_web_client_message(
            ClientToServer::AgentInput {
                content: serde_json::json!("while you were away"),
                send_mode: None,
                reasoning_effort: None,
                client_msg_id: Some(client_msg_id),
            },
            ctx,
            &mut Some(SessionId::from(session.to_string())),
            &mut Some(session),
            &mut HashMap::new(),
        );
    }

    /// The stages the sender saw for one input, with whether each was queued.
    fn stages(
        rx: &mut tokio::sync::mpsc::Receiver<ServerToClient>,
    ) -> Vec<(shared::InputDeliveryStage, bool)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|frame| match frame {
                ServerToClient::InputProgress { stage, queued, .. } => Some((stage, queued)),
                _ => None,
            })
            .collect()
    }

    /// With the agent offline, an input says it is queued only once it is
    /// durably stored; a row that fails to persist after a valid sequence
    /// bump never claims it.
    #[test]
    fn offline_input_is_queued_only_when_durably_stored() {
        use shared::InputDeliveryStage::ServerReceived;
        let Some(pool) = shared_pool() else { return };
        let state = test_app_state(pool);
        let mut conn = state.conn().unwrap();
        let owner = insert_user(&mut conn, "queued-owner");
        let session = insert_session(&mut conn, owner.id, "queued");
        let (tx, mut rx) = super::super::conn_channel(64);
        let (other_tx, mut other_rx) = super::super::conn_channel(64);
        state
            .session_manager
            .add_web_client(SessionId::from(session.id.to_string()), other_tx);

        send_input(&state, &tx, owner.id, session.id, Uuid::new_v4());
        assert_eq!(
            stages(&mut rx),
            [(ServerReceived, false), (ServerReceived, true)]
        );
        // The queued notice is the sender's alone.
        assert!(!std::iter::from_fn(|| other_rx.try_recv().ok())
            .any(|frame| matches!(frame, ServerToClient::InputProgress { queued: true, .. })));

        // Occupy the next sequence: the bump succeeds, the insert conflicts.
        let next: i64 = sessions::table
            .find(session.id)
            .select(sessions::input_seq)
            .first(&mut conn)
            .unwrap();
        diesel::insert_into(pending_inputs::table)
            .values(NewPendingInput {
                session_id: session.id,
                seq_num: next + 1,
                content: "\"squatter\"".into(),
                send_mode: "normal".into(),
                client_msg_id: None,
                reasoning_effort: None,
            })
            .execute(&mut conn)
            .unwrap();
        send_input(&state, &tx, owner.id, session.id, Uuid::new_v4());
        assert_eq!(stages(&mut rx), [(ServerReceived, false)]);
    }
}

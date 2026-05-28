use super::permissions::{handle_permission_response, replay_pending_permission};
use super::uploads::{handle_file_upload_chunk, handle_file_upload_start, PendingUpload};
use super::{SessionId, SessionManager, WebClientSender};
use crate::handlers::session_access::is_session_mutator;
use crate::models::NewPendingInput;
use crate::AppState;
use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{
    ClientEndpoint, ClientToServer, PortalMessage, SendMode, ServerToClient, ServerToProxy,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

pub async fn handle_web_client_socket(socket: WebSocket, app_state: Arc<AppState>, user_id: Uuid) {
    let session_manager = app_state.session_manager.clone();
    let db_pool = app_state.db_pool.clone();
    let initial_replay_limit = app_state.message_retention_count;
    let conn = ws_bridge::server::into_connection::<ClientEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerToClient>();

    let mut session_key: Option<SessionId> = None;
    let mut verified_session_id: Option<Uuid> = None;
    let mut pending_uploads: HashMap<String, PendingUpload> = HashMap::new();

    session_manager.add_user_client(user_id, tx.clone());

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    while let Some(result) = ws_receiver.recv().await {
        match result {
            Ok(client_msg) => {
                let should_break = handle_web_client_message(
                    client_msg,
                    &app_state,
                    &session_manager,
                    &db_pool,
                    &tx,
                    user_id,
                    initial_replay_limit,
                    &mut session_key,
                    &mut verified_session_id,
                    &mut pending_uploads,
                );
                if should_break {
                    break;
                }
            }
            Err(e) => {
                warn!("WebSocket decode error: {}", e);
                continue;
            }
        }
    }

    send_task.abort();
}

/// Returns true if the connection should be closed
#[allow(clippy::too_many_arguments)]
fn handle_web_client_message(
    client_msg: ClientToServer,
    app_state: &AppState,
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    tx: &WebClientSender,
    user_id: Uuid,
    initial_replay_limit: i64,
    session_key: &mut Option<SessionId>,
    verified_session_id: &mut Option<Uuid>,
    pending_uploads: &mut HashMap<String, PendingUpload>,
) -> bool {
    match client_msg {
        ClientToServer::Register(shared::RegisterFields {
            session_id,
            session_name,
            replay_after,
            ..
        }) => handle_web_register(
            app_state,
            session_manager,
            db_pool,
            tx,
            user_id,
            session_id,
            &session_name,
            replay_after,
            initial_replay_limit,
            session_key,
            verified_session_id,
        ),
        ClientToServer::ClaudeInput { content, send_mode } => {
            // Gate on editor/owner role — re-queried on each input so role
            // revocations take effect immediately for already-connected viewers.
            // See `session_access::is_session_mutator` for the layered rules.
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    let _ = tx.send(ServerToClient::Error {
                        message: "You don't have permission to send input to this session"
                            .to_string(),
                    });
                    return false;
                }
            }
            handle_web_input(
                session_manager,
                db_pool,
                session_key,
                *verified_session_id,
                content,
                send_mode,
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
        }) => {
            // File uploads end up as `ClaudeInput` on the proxy side — they're
            // mutations too, gate them on editor/owner role.
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    warn!(
                        "Viewer-role user {} attempted FileUploadStart on session {}",
                        user_id, session_id
                    );
                    return false;
                }
            }
            handle_file_upload_start(
                session_manager,
                session_key,
                pending_uploads,
                upload_id,
                filename,
                content_type,
                total_chunks,
                total_size,
                app_state.max_image_mb,
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
                    warn!(
                        "Viewer-role user {} attempted FileUploadChunk on session {}",
                        user_id, session_id
                    );
                    return false;
                }
            }
            handle_file_upload_chunk(
                session_manager,
                session_key,
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
                    warn!(
                        "Viewer-role user {} attempted PermissionResponse on session {}",
                        user_id, session_id
                    );
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
                        warn!(
                            "Viewer-role user {} attempted Interrupt on session {}",
                            user_id, session_id
                        );
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
    }
}

/// Handle web client registration. Returns true if the connection should be closed.
#[allow(clippy::too_many_arguments)]
fn handle_web_register(
    app_state: &AppState,
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    tx: &WebClientSender,
    user_id: Uuid,
    session_id: Uuid,
    session_name: &str,
    replay_after: Option<String>,
    initial_replay_limit: i64,
    session_key: &mut Option<SessionId>,
    verified_session_id: &mut Option<Uuid>,
) -> bool {
    match super::auth::verify_session_access(app_state, session_id, user_id) {
        Ok(_session) => {
            let key = session_id.to_string();
            *session_key = Some(key.clone());
            *verified_session_id = Some(session_id);

            session_manager.add_web_client(key, tx.clone());
            info!(
                "Web client connected to session: {} ({}) for user {}",
                session_name, session_id, user_id
            );

            replay_history(db_pool, tx, session_id, replay_after, initial_replay_limit);
            replay_pending_permission(db_pool, session_id, tx);
            false
        }
        Err(_) => {
            warn!(
                "User {} attempted to access session {} they don't own",
                user_id, session_id
            );
            let _ = tx.send(ServerToClient::Error {
                message: "Access denied: you don't own this session".to_string(),
            });
            true // close connection
        }
    }
}

/// Send historical messages from DB to a newly connected web client.
///
/// When the client supplies `replay_after`, we ship every message strictly
/// newer than that cursor — that's the reconnect / focus-regain path and the
/// frontend needs the full delta to stay consistent.
///
/// When `replay_after` is `None` (initial connection or hard refresh), we
/// only ship the trailing `initial_replay_limit` messages. Pre-#788 we
/// shipped the full session history every time and the frontend trimmed to
/// `MAX_MESSAGES_PER_SESSION = 100` locally — long sessions paid the full
/// `O(session_lifetime)` wire cost just to discard most of it. SQL now does
/// the trim. The DB query is `created_at DESC` with a `LIMIT` (so Postgres
/// returns the tail without sorting the whole table); the in-memory `.reverse()`
/// restores the chronological order `ServerToClient::HistoryBatch` consumers
/// expect (the frontend's `WsEvent::HistoryBatch` arm extends the message
/// vector and trims from the front).
fn replay_history(
    db_pool: &crate::db::DbPool,
    tx: &WebClientSender,
    session_id: Uuid,
    replay_after: Option<String>,
    initial_replay_limit: i64,
) {
    let mut conn = match db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!(
                "Failed to get database connection for history replay: {}",
                e
            );
            return;
        }
    };

    use crate::schema::messages;

    let replay_after_time = replay_after.as_ref().and_then(|ts| {
        chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.f")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S"))
            .ok()
    });

    let history: Vec<crate::models::Message> = if let Some(after) = replay_after_time {
        messages::table
            .filter(messages::session_id.eq(session_id))
            .filter(messages::created_at.gt(after))
            .order(messages::created_at.asc())
            .load(&mut conn)
            .unwrap_or_default()
    } else {
        let mut tail: Vec<crate::models::Message> = messages::table
            .filter(messages::session_id.eq(session_id))
            .order(messages::created_at.desc())
            .limit(initial_replay_limit)
            .load(&mut conn)
            .unwrap_or_default();
        tail.reverse();
        tail
    };

    info!(
        "Sending {} historical messages to web client (replay_after: {:?})",
        history.len(),
        replay_after
    );

    if history.is_empty() {
        return;
    }

    // Look up sender names for user-role messages
    use crate::schema::users;
    let user_ids: Vec<Uuid> = history
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.user_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let user_names: std::collections::HashMap<Uuid, String> = if !user_ids.is_empty() {
        users::table
            .filter(users::id.eq_any(&user_ids))
            .select((users::id, users::name, users::email))
            .load::<(Uuid, Option<String>, String)>(&mut conn)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, name, email)| (id, name.unwrap_or(email)))
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    // Surface the server-assigned `created_at` for the latest row so the
    // frontend can use it directly as its reconnect-replay watermark
    // without re-parsing the per-message `_created_at` injection below
    // (closes #784). History is ordered ASC, so `last()` is the newest.
    let last_created_at: Option<String> = history
        .last()
        .map(|msg| msg.created_at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string());

    let messages: Vec<serde_json::Value> = history
        .into_iter()
        .map(|msg| {
            let created_at_str = msg.created_at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string();
            // Fallback when the DB row content isn't valid JSON: wrap the raw
            // string in a typed envelope so the frontend's `ClaudeMessage`
            // dispatch still finds a `type` and `content`.
            #[derive(serde::Serialize)]
            struct FallbackMessageContent<'a> {
                #[serde(rename = "type")]
                message_type: &'a str,
                content: &'a str,
            }
            let mut val =
                serde_json::from_str::<serde_json::Value>(&msg.content).unwrap_or_else(|_| {
                    serde_json::to_value(FallbackMessageContent {
                        message_type: &msg.role,
                        content: &msg.content,
                    })
                    .unwrap_or(serde_json::Value::Null)
                });
            if let Some(obj) = val.as_object_mut() {
                // Reconstruct _sender for user messages from DB user_id
                if msg.role == "user" {
                    if let Some(name) = user_names.get(&msg.user_id) {
                        obj.insert(
                            "_sender".to_string(),
                            serde_json::json!({
                                "user_id": msg.user_id.to_string(),
                                "name": name,
                            }),
                        );
                    }
                }
                // Inject _created_at so renderers (and the watermark
                // recovery path on reload) see the server-assigned row
                // timestamp. Matches the REST list-messages path which
                // surfaces `created_at` on every `MessageWithSender`.
                obj.insert(
                    "_created_at".to_string(),
                    serde_json::Value::String(created_at_str),
                );
            }
            val
        })
        .collect();

    let _ = tx.send(ServerToClient::HistoryBatch {
        messages,
        last_created_at,
    });
}

fn handle_web_input(
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    session_key: &Option<SessionId>,
    verified_session_id: Option<Uuid>,
    content: serde_json::Value,
    send_mode: Option<SendMode>,
    user_id: Uuid,
) {
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
        use crate::schema::users;
        let display_name: String = users::table
            .find(user_id)
            .select(users::name)
            .first::<Option<String>>(&mut conn)
            .ok()
            .flatten()
            .or_else(|| {
                users::table
                    .find(user_id)
                    .select(users::email)
                    .first::<String>(&mut conn)
                    .ok()
            })
            .unwrap_or_else(|| "Unknown".to_string());
        session_manager
            .last_input_sender
            .insert(session_id, (user_id, display_name));
    }

    // For slash commands, broadcast a portal message so the user sees feedback
    if let serde_json::Value::String(text) = &content {
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

            // Insert first so the broadcast carries the server-assigned
            // `created_at` (closes #784 — same fix as handle_claude_output).
            let mut row_created_at: Option<String> = None;
            if let (Some(session), Ok(mut conn)) = (session.as_ref(), db_pool.get()) {
                use crate::schema::messages;
                let new_message = crate::models::NewMessage {
                    session_id,
                    role: "portal".to_string(),
                    content: serde_json::to_string(&portal.to_json()).unwrap_or_default(),
                    user_id,
                    agent_type: session.agent_type.clone(),
                };
                match diesel::insert_into(messages::table)
                    .values(&new_message)
                    .get_result::<crate::models::Message>(&mut conn)
                {
                    Ok(inserted) => {
                        row_created_at = Some(
                            inserted
                                .created_at
                                .format("%Y-%m-%dT%H:%M:%S%.6f")
                                .to_string(),
                        );
                    }
                    Err(e) => {
                        error!("Failed to store portal message: {}", e);
                    }
                }
            }

            session_manager.broadcast_to_web_clients(
                key,
                ServerToClient::ClaudeOutput {
                    content: portal.to_json(),
                    sender_user_id: None,
                    sender_name: None,
                    agent_type,
                    created_at: row_created_at,
                },
            );
        }
    }

    let seq = match db_pool.get() {
        Ok(mut conn) => {
            use crate::schema::{pending_inputs, sessions};

            let next_seq: i64 = diesel::update(sessions::table.find(session_id))
                .set(sessions::input_seq.eq(sessions::input_seq + 1))
                .returning(sessions::input_seq)
                .get_result(&mut conn)
                .unwrap_or(1);

            let new_input = NewPendingInput {
                session_id,
                seq_num: next_seq,
                content: serde_json::to_string(&content).unwrap_or_default(),
            };
            if let Err(e) = diesel::insert_into(pending_inputs::table)
                .values(&new_input)
                .execute(&mut conn)
            {
                error!("Failed to store pending input: {}", e);
            }
            next_seq
        }
        Err(e) => {
            error!("Failed to get db connection for pending input: {}", e);
            0
        }
    };

    if seq > 0 {
        if !session_manager.send_to_session(
            key,
            ServerToProxy::SequencedInput {
                session_id,
                seq,
                content,
                send_mode,
            },
        ) {
            warn!(
                "Failed to send to session '{}', session not found in SessionManager (input queued)",
                key
            );
        }
    } else if !session_manager
        .send_to_session(key, ServerToProxy::ClaudeInput { content, send_mode })
    {
        warn!(
            "Failed to send to session '{}', session not found in SessionManager",
            key
        );
    }
}

// =============================================================================
// DB-touching test for the initial-replay limit (closes #788).
//
// Mirrors the harness in `handlers::messages::db_tests` / `session_access::db_tests`:
// auto-skips when `DATABASE_URL` is not set so CI without a DB stays green.
// =============================================================================
#[cfg(test)]
mod replay_tests {
    use crate::models::{NewSessionWithId, NewUser, Session, User};
    use chrono::Utc;
    use diesel::prelude::*;
    use diesel::r2d2::{ConnectionManager, Pool};

    type DbPool = Pool<ConnectionManager<diesel::pg::PgConnection>>;

    fn try_pool() -> Option<DbPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let manager = ConnectionManager::<diesel::pg::PgConnection>::new(url);
        Pool::builder().build(manager).ok()
    }

    fn make_user(conn: &mut diesel::pg::PgConnection, label: &str) -> User {
        use crate::schema::users;
        let nonce = uuid::Uuid::new_v4();
        let new_user = NewUser {
            google_id: format!("test_replay_{}_{}", label, nonce),
            email: format!("test_replay_{}_{}@example.invalid", label, nonce),
            name: Some(format!("Test {}", label)),
            avatar_url: None,
        };
        diesel::insert_into(users::table)
            .values(&new_user)
            .get_result::<User>(conn)
            .expect("insert test user")
    }

    fn make_session(conn: &mut diesel::pg::PgConnection, owner_id: uuid::Uuid) -> Session {
        use crate::schema::sessions;
        let session_id = uuid::Uuid::new_v4();
        let new_session = NewSessionWithId {
            id: session_id,
            user_id: owner_id,
            session_name: format!("test-replay-{}", session_id),
            session_key: session_id.to_string(),
            working_directory: "/tmp".to_string(),
            status: "active".to_string(),
            git_branch: None,
            client_version: None,
            hostname: "test-host".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
        };
        diesel::insert_into(sessions::table)
            .values(&new_session)
            .get_result::<Session>(conn)
            .expect("insert test session")
    }

    fn seed_messages(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_id: uuid::Uuid,
        count: usize,
    ) -> Vec<chrono::NaiveDateTime> {
        use diesel::sql_query;
        let base = Utc::now().naive_utc();
        let mut stamps = Vec::with_capacity(count);
        for i in 0..count {
            let ts = base + chrono::Duration::microseconds((i as i64 + 1) * 1000);
            stamps.push(ts);
            sql_query(
                "INSERT INTO messages (id, session_id, role, content, created_at, user_id, agent_type)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind::<diesel::sql_types::Uuid, _>(uuid::Uuid::new_v4())
            .bind::<diesel::sql_types::Uuid, _>(session_id)
            .bind::<diesel::sql_types::VarChar, _>("assistant")
            .bind::<diesel::sql_types::Text, _>(format!("msg #{}", i))
            .bind::<diesel::sql_types::Timestamp, _>(ts)
            .bind::<diesel::sql_types::Uuid, _>(user_id)
            .bind::<diesel::sql_types::VarChar, _>("claude")
            .execute(conn)
            .expect("seed message");
        }
        stamps
    }

    fn cleanup(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_ids: &[uuid::Uuid],
    ) {
        use crate::schema::{messages, sessions, users};
        let _ = diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
            .execute(conn);
        let _ = diesel::delete(sessions::table.find(session_id)).execute(conn);
        for uid in user_ids {
            let _ = diesel::delete(users::table.find(uid)).execute(conn);
        }
    }

    /// `replay_after = None` returns at most `initial_replay_limit` rows in
    /// chronological order — the exact query shape `replay_history` builds.
    /// Pre-#788 this path loaded the full session history; now SQL trims to
    /// the render-window default so the wire payload is bounded.
    #[test]
    fn replay_after_none_caps_to_retention_count() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = make_user(&mut conn, "limit");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 200);

        let limit: i64 = 100;
        use crate::schema::messages;
        let mut tail: Vec<crate::models::Message> = messages::table
            .filter(messages::session_id.eq(session.id))
            .order(messages::created_at.desc())
            .limit(limit)
            .load(&mut conn)
            .expect("load");
        tail.reverse();

        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(tail.len(), limit as usize);
        // Tail is the newest 100, presented oldest-first.
        assert_eq!(tail.first().unwrap().created_at, stamps[100]);
        assert_eq!(tail.last().unwrap().created_at, stamps[199]);
        for w in tail.windows(2) {
            assert!(w[0].created_at < w[1].created_at);
        }
    }

    // -----------------------------------------------------------------
    // #784: server-assigned replay watermark
    //
    // `replay_history` parses the client-supplied `replay_after` ISO timestamp,
    // then runs `messages.created_at.gt(after)`. The silent-data-loss chain
    // was: frontend stored `Date.now()` as `last_message_timestamp`, sent
    // it as `replay_after`, and the backend filtered out perfectly-good
    // rows whose `created_at` happened to land BEFORE the browser's
    // clock-skewed "now". The wire-shape fix moves the watermark source
    // onto the server so the round-trip is now `server-assigned created_at
    // → frontend stores it verbatim → frontend sends it back → server
    // filters strictly greater than it`. These tests pin the backend half
    // of that round-trip: the precise format we write into the wire
    // (microsecond precision) must parse on the way back in, and the
    // strict-`>` semantics must hold so a message whose `created_at`
    // *equals* the watermark is treated as already seen.

    /// Capture the `ServerToClient` messages a `replay_history` invocation
    /// pushes onto its sender so we can assert against the new
    /// `HistoryBatch.last_created_at` + per-message `_created_at`
    /// injection without standing up a full WS round-trip.
    fn capture_sender() -> (
        super::WebClientSender,
        tokio::sync::mpsc::UnboundedReceiver<shared::ServerToClient>,
    ) {
        tokio::sync::mpsc::unbounded_channel::<shared::ServerToClient>()
    }

    /// The fix's whole point: `replay_history` with a `replay_after`
    /// equal to a previously-broadcast row's `created_at` must return
    /// ONLY messages strictly after it. Seed three rows, take the middle
    /// row's server-assigned `created_at` (formatted exactly the way the
    /// broadcast path formats it), pass it as `replay_after`, and assert:
    /// the watermark row itself is excluded, the earlier row is excluded,
    /// the later row is included. This is the round-trip the frontend
    /// now performs end-to-end (#784).
    #[test]
    fn replay_history_strict_gt_round_trips_server_timestamp() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping replay_history test");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = make_user(&mut conn, "strict_gt");
        let session = make_session(&mut conn, user.id);
        // Three rows with strictly increasing server-assigned timestamps.
        let stamps = seed_messages(&mut conn, session.id, user.id, 3);
        drop(conn);

        // Format the watermark exactly the way the broadcast path
        // formats `created_at` — microsecond precision, no timezone
        // suffix. That's the string the frontend will store and replay.
        let watermark = stamps[1].format("%Y-%m-%dT%H:%M:%S%.6f").to_string();
        let expected_last = stamps[2].format("%Y-%m-%dT%H:%M:%S%.6f").to_string();

        let (tx, mut rx) = capture_sender();
        // initial_replay_limit is unused on the `replay_after = Some(_)`
        // branch — that branch loads strictly-newer rows unbounded.
        super::replay_history(&pool, &tx, session.id, Some(watermark), 100);
        drop(tx);

        let mut got: Option<shared::ServerToClient> = None;
        while let Ok(msg) = rx.try_recv() {
            got = Some(msg);
        }
        let batch = got.expect("replay_history must send exactly one HistoryBatch");

        let (messages, last_created_at) = match batch {
            shared::ServerToClient::HistoryBatch {
                messages,
                last_created_at,
            } => (messages, last_created_at),
            other => panic!("expected HistoryBatch, got {:?}", other),
        };

        // The strict-`>` predicate must exclude stamp[0] (older) and
        // stamp[1] (equal-to-watermark) and include stamp[2] (newer).
        // The exact-equal exclusion is the contract that closes the
        // silent-data-loss window in #784 — a frontend that already
        // rendered the watermark row is told by its watermark "I have
        // everything up to and including stamp[1]", and the backend
        // honors that semantic.
        let mut conn = pool.get().expect("conn");
        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(
            messages.len(),
            1,
            "expected only the newest row to be replayed; got {} messages: {:?}",
            messages.len(),
            messages
        );
        assert_eq!(last_created_at.as_deref(), Some(expected_last.as_str()));
        let injected = messages[0]
            .get("_created_at")
            .and_then(|v| v.as_str())
            .expect("each replayed message must carry _created_at");
        assert_eq!(injected, expected_last);
    }

    /// A `None` `replay_after` (initial connect with no prior watermark)
    /// must return up to `initial_replay_limit` rows and carry
    /// `last_created_at` set to the newest row's timestamp so the frontend
    /// can start its watermark from there. Pins the initial-connect path
    /// the component takes when REST history returned empty / failed.
    #[test]
    fn replay_history_no_watermark_returns_all_and_advances_high_water() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping replay_history test");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = make_user(&mut conn, "no_watermark");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 3);
        drop(conn);

        let (tx, mut rx) = capture_sender();
        super::replay_history(&pool, &tx, session.id, None, 100);
        drop(tx);

        let mut batch: Option<shared::ServerToClient> = None;
        while let Ok(msg) = rx.try_recv() {
            batch = Some(msg);
        }
        let batch = batch.expect("replay_history must send HistoryBatch");

        let (messages, last_created_at) = match batch {
            shared::ServerToClient::HistoryBatch {
                messages,
                last_created_at,
            } => (messages, last_created_at),
            other => panic!("expected HistoryBatch, got {:?}", other),
        };

        let mut conn = pool.get().expect("conn");
        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(messages.len(), 3, "expected all rows to be replayed");
        let expected_last = stamps[2].format("%Y-%m-%dT%H:%M:%S%.6f").to_string();
        assert_eq!(last_created_at.as_deref(), Some(expected_last.as_str()));
    }
}

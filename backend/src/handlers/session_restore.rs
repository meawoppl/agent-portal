//! Owner-only restore of an archived session from the history browser.
//!
//! A deleted session's native agent conversation (Claude transcript or Codex
//! thread) usually survives on the launcher host, because deleting a session
//! never touches native state. Restoring creates a new, paused Portal session,
//! has the launcher bind that conversation to it, and copies the archived
//! transcript in, so **Resume** continues the original conversation with its
//! full context. Only full restores exist: if the native conversation is gone
//! the restore is refused rather than starting a fresh agent.
use crate::archive::{
    decode_transcript, transcript_key, SessionArchiveManifest, TRANSCRIPT_COMPRESSION,
};
use crate::auth::extract_user;
use crate::db::session_lifecycle::{create_desired_session, DesiredSessionDraft};
use crate::handlers::history::{archive_runtime, on_blocking};
use crate::handlers::launcher_rpc::launcher_rpc;
use crate::schema::{messages, sessions};
use crate::{errors::AppError, AppState};
use archive_format::ArchiveMessageLine;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use diesel::prelude::*;
use shared::api::{
    restored_session_id, RestoreSessionResponse, MAX_IMPORT_HISTORY_BYTES, MAX_IMPORT_MESSAGES,
};
use shared::{AgentType, LauncherToServer, ServerToLauncher};
use std::sync::Arc;
use tower_cookies::Cookies;
use uuid::Uuid;

/// POST /api/history/sessions/{user}/{session}/restore
pub async fn restore_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Path((owner, session)): Path<(Uuid, Uuid)>,
) -> Result<Json<RestoreSessionResponse>, AppError> {
    let caller = extract_user(&state, Some(&headers), &cookies)?;
    // Restoring rebinds the owner's native conversation on the owner's own
    // computer, so it is owner-only even for admins and share members.
    if caller.id != owner {
        return Err(AppError::NotFound("Session not found"));
    }

    if session_exists(&mut *state.conn()?, session, owner)? {
        return Ok(Json(RestoreSessionResponse {
            session_id: session,
            already_live: true,
            already_restored: false,
        }));
    }
    let restored_id = restored_session_id(owner, session);
    if session_exists(&mut *state.conn()?, restored_id, owner)? {
        return Ok(Json(RestoreSessionResponse {
            session_id: restored_id,
            already_live: false,
            already_restored: true,
        }));
    }

    let runtime = archive_runtime(&state)?;
    let (manifest, lines) = on_blocking(move || {
        let Some(manifest) = runtime.store.get_session_manifest(owner, session)? else {
            return Ok(None);
        };
        let lines = match runtime.store.get_object(&transcript_key(owner, session))? {
            Some(raw) => {
                let compression = manifest
                    .transcript
                    .as_ref()
                    .map(|t| t.compression.clone())
                    .unwrap_or_else(|| TRANSCRIPT_COMPRESSION.to_string());
                archive_format::parse_transcript_ndjson(&decode_transcript(&compression, &raw)?)?
            }
            None => Vec::new(),
        };
        Ok(Some((manifest, lines)))
    })
    .await?
    .ok_or(AppError::NotFound("Session not found"))?;

    let agent_type = match manifest.agent_type.parse::<AgentType>() {
        Ok(agent @ (AgentType::Claude | AgentType::Codex)) => agent,
        _ => {
            return Err(AppError::BadRequest(
                "Only Claude and Codex sessions can be restored",
            ))
        }
    };
    let launcher = restore_launcher(&state, &manifest)?;

    let request_id = Uuid::new_v4();
    let reply = launcher_rpc(
        &state,
        launcher,
        request_id,
        ServerToLauncher::PrepareSessionRestore {
            request_id,
            agent_type,
            source_session_id: session,
            session_id: restored_id,
            working_directory: manifest.working_directory.clone(),
        },
        30,
    )
    .await?;
    let LauncherToServer::PrepareSessionRestoreResult { result, .. } = reply else {
        return Err(AppError::BadGateway("Unexpected restore response"));
    };
    result.map_err(AppError::BadGatewayMessage)?;

    let (hostname, version) = state
        .session_manager
        .launcher_host_version(launcher)
        .ok_or(AppError::NotFound("Launcher disconnected during restore"))?;
    let draft = DesiredSessionDraft {
        session_id: restored_id,
        user_id: owner,
        working_directory: manifest.working_directory.clone(),
        session_name: manifest.session_name.clone(),
        hostname,
        launcher_id: Some(launcher),
        client_version: Some(version),
        launcher_version: state.session_manager.launcher_version(Some(launcher)),
        agent_type,
        claude_args: manifest.claude_args.clone(),
        forked_from_session_id: None,
        fork_point_turn_id: None,
        fork_create_worktree: false,
    };
    let mut conn = state.conn()?;
    let already_restored = match store_restore(&mut conn, draft, session, lines) {
        Ok(()) => false,
        // The deterministic id serializes concurrent restores; the loser's
        // transaction (messages included) rolled back.
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) if session_exists(&mut conn, restored_id, owner)? => true,
        Err(e) => return Err(e.into()),
    };
    Ok(Json(RestoreSessionResponse {
        session_id: restored_id,
        already_live: false,
        already_restored,
    }))
}

fn session_exists(conn: &mut PgConnection, id: Uuid, user: Uuid) -> QueryResult<bool> {
    diesel::select(diesel::dsl::exists(
        sessions::table
            .filter(sessions::id.eq(id))
            .filter(sessions::user_id.eq(user)),
    ))
    .get_result(conn)
}

/// The launcher the session last ran on, or another connected launcher of the
/// owner's on the same host. The native conversation lives on that host.
fn restore_launcher(state: &AppState, manifest: &SessionArchiveManifest) -> Result<Uuid, AppError> {
    let manager = &state.session_manager;
    let launcher = manifest
        .launcher_id
        .filter(|id| manager.launcher_owner(*id) == Some(manifest.user_id))
        .or_else(|| manager.find_launcher_for_user_host(manifest.user_id, &manifest.hostname))
        .ok_or(AppError::ServiceUnavailable(
            "The computer this session ran on is not connected",
        ))?;
    if !manager.launcher_supports_capability(launcher, shared::LAUNCHER_CAPABILITY_RESTORE_SESSION)
    {
        return Err(AppError::BadRequest(
            "Update agent-portal on the computer this session ran on to restore it",
        ));
    }
    Ok(launcher)
}

/// The most recent archived lines that fit the import limits. Scrollback only:
/// the agent's own context comes from the native conversation, so trimming
/// old display history loses nothing the agent needs.
fn recent_within_limits(lines: Vec<ArchiveMessageLine>) -> (Vec<ArchiveMessageLine>, bool) {
    let total = lines.len();
    let mut kept = Vec::new();
    let mut bytes = 0;
    for line in lines.into_iter().rev() {
        bytes += line.content.to_string().len();
        if bytes > MAX_IMPORT_HISTORY_BYTES || kept.len() >= MAX_IMPORT_MESSAGES {
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    let trimmed = kept.len() < total;
    (kept, trimmed)
}

/// Stored message content is JSON text; the archive embeds it as a value (a
/// bare string when the original was not JSON).
fn stored_content(content: serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }
}

fn store_restore(
    conn: &mut PgConnection,
    draft: DesiredSessionDraft,
    source_session_id: Uuid,
    lines: Vec<ArchiveMessageLine>,
) -> QueryResult<()> {
    let id = draft.session_id;
    let user = draft.user_id;
    let (lines, trimmed) = recent_within_limits(lines);
    conn.transaction(|conn| {
        create_desired_session(conn, draft)?;
        crate::db::session_lifecycle::pause(conn, id)?;
        let mut rows: Vec<_> = lines
            .into_iter()
            .map(|line| crate::models::NewMessage {
                session_id: id,
                user_id: user,
                role: line.role,
                content: stored_content(line.content),
                agent_type: line.agent_type,
                provenance_kind: None,
                provenance_session_id: None,
                provenance_agent_type: None,
            })
            .collect();
        let scrollback = if trimmed {
            "The most recent part of the transcript was copied; the agent still has the whole conversation."
        } else {
            "The transcript was copied from history."
        };
        rows.push(crate::models::NewMessage {
            session_id: id,
            user_id: user,
            role: "assistant".into(),
            agent_type: rows
                .last()
                .map(|row| row.agent_type.clone())
                .unwrap_or_default(),
            content: shared::PortalMessage::text(format!(
                "**Restored from history** (session `{}`)\n\n{scrollback} This session is paused; use **Resume** to continue the original conversation.",
                shared::short_uuid(&source_session_id)
            ))
            .to_json()
            .to_string(),
            provenance_kind: None,
            provenance_session_id: None,
            provenance_agent_type: None,
        });
        // Stay below PostgreSQL's bind-parameter limit on long transcripts.
        for chunk in rows.chunks(500) {
            diesel::insert_into(messages::table)
                .values(chunk)
                .execute(conn)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> ArchiveMessageLine {
        ArchiveMessageLine {
            id: Uuid::new_v4(),
            role: "user".into(),
            created_at: chrono::NaiveDateTime::default(),
            agent_type: "claude".into(),
            content: serde_json::Value::String(text.into()),
        }
    }

    #[test]
    fn recent_lines_are_kept_in_order() {
        let (kept, trimmed) = recent_within_limits(vec![line("a"), line("b"), line("c")]);
        assert!(!trimmed);
        let texts: Vec<_> = kept.iter().map(|l| l.content.clone()).collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
    }

    #[test]
    fn oversized_history_keeps_the_newest_lines() {
        let big = "x".repeat(MAX_IMPORT_HISTORY_BYTES / 2);
        let (kept, trimmed) =
            recent_within_limits(vec![line("oldest"), line(&big), line(&big), line("newest")]);
        assert!(trimmed);
        assert_eq!(kept.last().unwrap().content, "newest");
        assert!(kept.iter().all(|l| l.content != "oldest"));
    }

    #[test]
    fn json_content_round_trips_to_stored_text() {
        let value = serde_json::json!({"type": "user", "content": "hi"});
        assert_eq!(stored_content(value.clone()), value.to_string());
        assert_eq!(
            stored_content(serde_json::Value::String("raw".into())),
            "raw"
        );
    }
}

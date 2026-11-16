//! Cross-agent handoff: continue a session's work with a different agent.
//!
//! Agent conversation state is native to each agent (a Codex thread, a Claude
//! session file) and cannot be loaded by another, so a handoff cannot be a
//! fork. Instead it launches a fresh session of the target agent in the same
//! place and queues a handoff note as its first input. The note carries a short
//! orientation (the last request and reply) and tells the new agent how to read
//! the source's complete Portal transcript with `agent-portal message history`.
//! Everything is built from what Portal already stores, so the source agent —
//! typically out of usage — never has to write anything.
//!
//! The source is stopped and paused by default: two agents in one checkout
//! overwrite each other, and a usage-limited Claude resumes on its own when its
//! window resets.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use diesel::prelude::*;
use shared::api::{
    ForkDirectoryMode, HandoffSessionRequest, HandoffSessionResponse, LaunchRequest, LaunchSpec,
};
use shared::{AgentType, WorktreeMode};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::AppState;

/// How far back to look for the last request and reply. Usage-limit errors
/// and tool chatter sit between them, but a few hundred rows is plenty.
const ORIENTATION_SCAN_ROWS: i64 = 400;

/// POST /api/sessions/:session_id/handoff
pub async fn handoff_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(source_id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<HandoffSessionRequest>,
) -> Result<Json<HandoffSessionResponse>, AppError> {
    crate::auth::require_manual_launch_credential(&app_state, &headers)?;
    use crate::schema::{messages, sessions};

    let mut conn = app_state.conn()?;
    let source = sessions::table
        .find(source_id)
        .filter(sessions::user_id.eq(user_id))
        .first::<crate::models::Session>(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("Source session not found"))?;
    let launcher_id = source.launcher_id.ok_or(AppError::BadRequest(
        "Proxy-direct sessions cannot be handed off",
    ))?;
    let source_agent = AgentType::parse_or_default(&source.agent_type);
    if source_agent == req.target_agent {
        return Err(AppError::BadRequest(
            "Handoff needs a different agent; fork the session to continue with the same one",
        ));
    }

    let (working_directory, worktree) = match req.directory_mode {
        ForkDirectoryMode::Same => (source.working_directory.clone(), WorktreeMode::None),
        ForkDirectoryMode::Worktree => (
            source.working_directory.clone(),
            WorktreeMode::Repo {
                branch: shared::strings::trimmed_non_blank(Some(&req.name)).map(str::to_string),
            },
        ),
        ForkDirectoryMode::Other => (
            req.working_directory
                .clone()
                .filter(|path| shared::strings::is_non_blank(path))
                .ok_or(AppError::BadRequest(
                    "working_directory is required for other-directory handoffs",
                ))?,
            WorktreeMode::None,
        ),
    };

    let rows: Vec<(String, String, String)> = messages::table
        .filter(messages::session_id.eq(source_id))
        .order((messages::created_at.desc(), messages::id.desc()))
        .limit(ORIENTATION_SCAN_ROWS)
        .select((messages::role, messages::agent_type, messages::content))
        .load(&mut conn)?;
    let orientation = Orientation::from_newest_first(&rows);
    drop(conn);

    let launched = super::launchers::launch_owned_session(
        &app_state,
        user_id,
        LaunchRequest {
            launch: LaunchSpec {
                working_directory,
                session_name: Some(req.name.clone()),
                claude_args: req.claude_args.clone(),
                agent_type: req.target_agent,
                worktree,
            },
            launcher_id: Some(launcher_id),
            name: None,
            create_worktree: false,
            plugin_selection: None,
        },
        None,
    )?;

    let note = handoff_note(&HandoffNote {
        source_name: &source.session_name,
        source_id,
        source_agent,
        source_directory: &source.working_directory,
        new_checkout: req.directory_mode != ForkDirectoryMode::Same,
        source_stopped: req.stop_source,
        orientation: &orientation,
        instructions: req.instructions.as_deref(),
    });
    app_state.session_manager.set_last_input_sender(
        launched.session_id,
        user_id,
        "Handoff notice".to_string(),
    );
    app_state.session_manager.enqueue_input(
        &app_state.db_pool,
        &launched.session_id.to_string(),
        launched.session_id,
        crate::handlers::websocket::EnqueueInput {
            content: serde_json::Value::String(note),
            send_mode: None,
            reasoning_effort: None,
            client_msg_id: None,
        },
    );

    // The new session is already launching, so a failed stop is reported to
    // the caller rather than failing the request.
    let source_stopped = req.stop_source
        && app_state
            .conn()
            .and_then(|mut conn| {
                super::session_stop::persist_and_stop(
                    &mut conn,
                    &app_state.session_manager,
                    &source,
                )
                .map_err(AppError::from)
            })
            .inspect_err(|e| warn!("Handoff from {source_id}: failed to stop source: {e:?}"))
            .is_ok();

    Ok(Json(HandoffSessionResponse {
        request_id: launched.request_id,
        session_id: launched.session_id,
        source_stopped,
    }))
}

/// The newest human request and agent reply, as one-line excerpts.
#[derive(Debug, Default, PartialEq)]
struct Orientation {
    last_request: Option<String>,
    last_reply: Option<String>,
}

impl Orientation {
    /// `rows` are `(role, agent_type, content)`, newest first. The reply is
    /// the newest agent text at any position: after a usage-limit failure the
    /// newest request often has no reply yet, and the previous one still
    /// tells the new agent where things stood.
    fn from_newest_first(rows: &[(String, String, String)]) -> Self {
        let mut found = Self::default();
        for (role, agent, content) in rows {
            if found.last_request.is_some() && found.last_reply.is_some() {
                break;
            }
            let summary = super::peek_summary::summarize_message(
                Uuid::nil(),
                agent,
                role,
                content,
                chrono::NaiveDateTime::default(),
            );
            if summary.kind != "text" || summary.summary.trim().is_empty() {
                continue;
            }
            let slot = if role == "user" {
                &mut found.last_request
            } else {
                &mut found.last_reply
            };
            slot.get_or_insert(summary.summary);
        }
        found
    }
}

struct HandoffNote<'a> {
    source_name: &'a str,
    source_id: Uuid,
    source_agent: AgentType,
    source_directory: &'a str,
    new_checkout: bool,
    source_stopped: bool,
    orientation: &'a Orientation,
    instructions: Option<&'a str>,
}

fn handoff_note(n: &HandoffNote<'_>) -> String {
    let id = n.source_id;
    let short = shared::short_uuid(&id);
    let agent = n.source_agent.display_name();
    let mut note = format!(
        "You are taking over work from Agent Portal session \"{}\" ({id}), a {agent} session that \
         could not continue (usually because it ran out of usage). ",
        n.source_name
    );
    note.push_str(if n.source_stopped {
        "That session has been stopped, so you are the only agent on this work.\n\n"
    } else {
        "That session was left running; coordinate before changing shared files.\n\n"
    });
    note.push_str(&format!(
        "Read its transcript before acting:\n\
         - `agent-portal message peek {id} -n 40` for a quick look at the latest activity.\n\
         - `agent-portal message history {id} -o /tmp/handoff-{short}.history.ndjson` for the \
         complete transcript as NDJSON. It is large; search it rather than reading it whole.\n\n"
    ));
    if n.new_checkout {
        note.push_str(&format!(
            "You are in a different checkout from the source, which worked in `{}`. Its \
             uncommitted changes are not here; inspect that directory if you need them.\n\n",
            n.source_directory
        ));
    } else {
        note.push_str(
            "You are in the same directory the source worked in. Check `git status` and \
             `git diff` first: it may have left work half-done.\n\n",
        );
    }
    if let Some(request) = &n.orientation.last_request {
        note.push_str(&format!(
            "The user's most recent request began: \"{request}\"\n"
        ));
    }
    if let Some(reply) = &n.orientation.last_reply {
        note.push_str(&format!(
            "The source agent's last reply began: \"{reply}\"\n"
        ));
    }
    if n.orientation.last_request.is_some() || n.orientation.last_reply.is_some() {
        note.push('\n');
    }
    note.push_str(
        shared::strings::trimmed_non_blank(n.instructions).unwrap_or(
            "Continue the user's most recent request from where the source agent stopped. \
             Briefly tell the user what state you found before you carry on.",
        ),
    );
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, agent: &str, content: &str) -> (String, String, String) {
        (role.into(), agent.into(), content.into())
    }

    const CODEX_REPLY: &str = r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"Tests pass; next I will wire the button."}}"#;

    #[test]
    fn orientation_finds_newest_request_and_newest_reply_across_a_failed_turn() {
        let rows = [
            row(
                "unknown",
                "codex",
                r#"{"type":"turn.failed","error":{"message":"usage limit"}}"#,
            ),
            row(
                "user",
                "codex",
                r#"{"type":"user","content":"Now add the button"}"#,
            ),
            row("unknown", "codex", CODEX_REPLY),
            row(
                "user",
                "codex",
                r#"{"type":"user","content":"Older request"}"#,
            ),
        ];
        let found = Orientation::from_newest_first(&rows);
        assert_eq!(found.last_request.as_deref(), Some("Now add the button"));
        assert_eq!(
            found.last_reply.as_deref(),
            Some("Tests pass; next I will wire the button.")
        );
    }

    #[test]
    fn orientation_is_empty_without_text() {
        assert_eq!(Orientation::from_newest_first(&[]), Orientation::default());
    }

    fn note(new_checkout: bool, stopped: bool, instructions: Option<&str>) -> String {
        handoff_note(&HandoffNote {
            source_name: "research",
            source_id: Uuid::nil(),
            source_agent: AgentType::Codex,
            source_directory: "/home/u/repo",
            new_checkout,
            source_stopped: stopped,
            orientation: &Orientation {
                last_request: Some("Fix the parser".into()),
                last_reply: None,
            },
            instructions,
        })
    }

    #[test]
    fn note_points_at_the_transcript_and_the_checkout() {
        let same = note(false, true, None);
        assert!(same.contains("a Codex session"));
        assert!(same.contains(&format!("agent-portal message history {}", Uuid::nil())));
        assert!(same.contains("git status"));
        assert!(same.contains("has been stopped"));
        assert!(same.contains("\"Fix the parser\""));
        assert!(!same.contains("last reply"));
        assert!(same.ends_with("before you carry on."));

        let other = note(true, false, Some("Only review the diff."));
        assert!(other.contains("`/home/u/repo`"));
        assert!(other.contains("left running"));
        assert!(other.ends_with("Only review the diff."));
    }
}

//! Speech-to-text endpoint: recorded audio in, transcript out.
//!
//! One request per utterance. Streaming would buy live interim text, but it
//! would also mean re-introducing an audio WebSocket and a PCM `AudioWorklet` —
//! the plumbing that made the previous server-STT attempt expensive to own. A
//! push-to-talk round trip is a fraction of that and covers the browsers the
//! Web Speech path cannot reach at all.

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{header, HeaderMap},
    Json,
};
use diesel::prelude::*;
use serde::Deserialize;
use shared::api::TranscriptionResponse;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::AppState;
use portal_stt::{session_keyterms, TranscribeRequest};

#[derive(Debug, Deserialize)]
pub struct TranscribeQuery {
    /// Session to draw vocabulary hints from. Optional — transcription works
    /// without it, just less accurately on project-specific words.
    #[serde(default)]
    pub session_id: Option<Uuid>,
    /// BCP-47 language tag from the browser, when it knows one.
    #[serde(default)]
    pub language: Option<String>,
}

/// `POST /api/stt/transcribe` — body is the raw recording.
pub async fn transcribe(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Query(query): Query<TranscribeQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<TranscriptionResponse>, AppError> {
    let provider = app_state.stt.as_ref().ok_or(AppError::ServiceUnavailable(
        "Speech-to-text is not configured",
    ))?;

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(AppError::BadRequest("missing Content-Type header"))?;
    if !content_type.starts_with("audio/") {
        return Err(AppError::BadRequest("Content-Type must be an audio type"));
    }

    if body.is_empty() {
        return Err(AppError::BadRequest("empty audio body"));
    }
    let cap = app_state.max_audio_mb as u64 * 1024 * 1024;
    if body.len() as u64 > cap {
        return Err(AppError::PayloadTooLarge(format!(
            "{:.1} MB exceeds the {} MB limit for audio",
            body.len() as f64 / (1024.0 * 1024.0),
            app_state.max_audio_mb,
        )));
    }

    // Vocabulary hints, when the caller named a session they belong to and the
    // provider can actually use them — several vendors bias through a trained
    // model instead, and there is no point paying for the query to build a list
    // nobody reads. A session the user is *not* a member of contributes nothing
    // rather than erroring: the transcript is still useful, and this keeps the
    // endpoint from doubling as a membership oracle.
    let keyterms = match query.session_id {
        Some(session_id) if provider.supports_keyterms() => {
            keyterms_for_session(&app_state, user_id, session_id)?
        }
        _ => Vec::new(),
    };

    let audio_len = body.len();
    let transcript = provider
        .transcribe(TranscribeRequest {
            audio: body,
            content_type,
            language: query.language.as_deref(),
            keyterms: &keyterms,
        })
        .await
        .map_err(|e| {
            warn!(
                target: "stt",
                event = "transcribe_failed",
                provider = provider.key(),
                error = %e,
            );
            AppError::BadGateway("Speech provider could not transcribe the audio")
        })?;

    info!(
        target: "stt",
        event = "transcribe_ok",
        provider = provider.key(),
        audio_bytes = audio_len,
        keyterms = keyterms.len(),
        transcript_chars = transcript.chars().count(),
    );

    Ok(Json(TranscriptionResponse { text: transcript }))
}

/// Keyterms for a session the user is a member of; empty otherwise.
fn keyterms_for_session(
    app_state: &AppState,
    user_id: Uuid,
    session_id: Uuid,
) -> Result<Vec<String>, AppError> {
    use crate::schema::{session_members, sessions};

    let mut conn = app_state.conn()?;
    let row: Option<(String, Option<String>, Option<String>, String)> = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select((
            sessions::working_directory,
            sessions::git_branch,
            sessions::repo_url,
            sessions::agent_type,
        ))
        .first(&mut conn)
        .optional()?;

    Ok(match row {
        Some((working_directory, git_branch, repo_url, agent_type)) => session_keyterms(
            &working_directory,
            git_branch.as_deref(),
            repo_url.as_deref(),
            &agent_type,
        ),
        None => Vec::new(),
    })
}

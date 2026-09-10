use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::strings::is_non_blank;
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::header,
    response::IntoResponse,
};
use base64::Engine;
use diesel::prelude::*;
use serde::Deserialize;
use shared::{FileDownloadRequestFields, ServerToProxy};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Max on-disk size for a `portal://file/` pull. Must stay deliverable through
/// the session socket: the proxy answers with one base64 text message, so this
/// cap ×4/3 (+ envelope) must fit under
/// [`super::websocket::SESSION_WS_MAX_MESSAGE_BYTES`] — pinned by
/// `pull_cap_fits_the_session_socket` below.
const MAX_PULL_BYTES: u64 = 25 * 1024 * 1024;
const PULL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
pub struct PullFileQuery {
    pub path: String,
}

pub async fn pull_session_file(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
    Query(query): Query<PullFileQuery>,
) -> Result<impl IntoResponse, AppError> {
    if !is_non_blank(&query.path) {
        return Err(AppError::BadRequest("path is required"));
    }

    verify_session_read_access(&app_state, session_id, current_user_id)?;

    let request_id = Uuid::new_v4();
    let rx = app_state.session_manager.register_file_download(request_id);

    let sent = app_state.session_manager.send_to_connected_session(
        &session_id.to_string(),
        ServerToProxy::FileDownloadRequest(FileDownloadRequestFields {
            request_id,
            path: query.path,
            max_bytes: MAX_PULL_BYTES,
        }),
    );

    if !sent {
        app_state.session_manager.cancel_file_download(request_id);
        return Err(AppError::ServiceUnavailable(
            "Session proxy is not connected",
        ));
    }

    let response = match tokio::time::timeout(PULL_TIMEOUT, rx).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => return Err(AppError::BadGateway("File download response was dropped")),
        Err(_) => {
            app_state.session_manager.cancel_file_download(request_id);
            return Err(AppError::GatewayTimeout("File download timed out"));
        }
    };

    if !response.success {
        return Err(AppError::NotFound("File not available"));
    }

    let data_base64 = response
        .data_base64
        .ok_or(AppError::BadGateway("File response missing data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|_| AppError::BadGateway("File response was not valid base64"))?;

    if bytes.len() as u64 > MAX_PULL_BYTES {
        return Err(AppError::BadGateway("File response exceeded size limit"));
    }

    let filename = response
        .filename
        .as_deref()
        .map(sanitize_download_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download".to_string());
    let content_type = response
        .media_type
        .filter(|s| is_non_blank(s))
        .unwrap_or_else(|| "application/octet-stream".to_string());

    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        bytes,
    ))
}

fn verify_session_read_access(
    app_state: &AppState,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<(), AppError> {
    let mut conn = app_state.conn()?;
    use crate::schema::{session_members, sessions};

    let is_owner = sessions::table
        .filter(sessions::id.eq(session_id))
        .filter(sessions::user_id.eq(user_id))
        .select(sessions::id)
        .first::<Uuid>(&mut conn)
        .optional()?
        .is_some();
    if is_owner {
        return Ok(());
    }

    session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(session_members::id)
        .first::<Uuid>(&mut conn)
        .optional()?
        .map(|_| ())
        .ok_or(AppError::NotFound("Session not found"))
}

fn sanitize_download_filename(filename: &str) -> String {
    filename
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '\0' | '"' | '\r' | '\n'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_download_filename_strips_header_and_path_separators() {
        assert_eq!(
            sanitize_download_filename("../bad\"name\n.txt"),
            "..badname.txt"
        );
    }

    /// The invariant that made #ecba289d's downloads kill the session socket:
    /// a max-size pull, base64-encoded into one FileDownloadResponse message
    /// (plus generous envelope headroom), must fit under the session socket's
    /// frame/message ceiling — otherwise the backend closes the connection on
    /// exactly the files the cap says are allowed.
    #[test]
    fn pull_cap_fits_the_session_socket() {
        let base64_len = MAX_PULL_BYTES.div_ceil(3) * 4;
        let envelope_headroom = 64 * 1024;
        assert!(
            base64_len + envelope_headroom
                < crate::handlers::websocket::SESSION_WS_MAX_MESSAGE_BYTES as u64,
            "MAX_PULL_BYTES ({MAX_PULL_BYTES}) base64s to {base64_len}, exceeding the              session socket ceiling — raise SESSION_WS_MAX_MESSAGE_BYTES or lower the cap"
        );
    }
}

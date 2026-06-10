use crate::auth::extract_user_id;
use crate::errors::AppError;
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
use tokio::sync::oneshot;
use uuid::Uuid;

const MAX_PULL_BYTES: u64 = 25 * 1024 * 1024;
const PULL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
pub struct PullFileQuery {
    pub path: String,
}

pub async fn pull_session_file(
    State(app_state): State<Arc<AppState>>,
    cookies: tower_cookies::Cookies,
    Path(session_id): Path<Uuid>,
    Query(query): Query<PullFileQuery>,
) -> Result<impl IntoResponse, AppError> {
    if query.path.trim().is_empty() {
        return Err(AppError::BadRequest("path is required"));
    }

    let current_user_id = extract_user_id(&app_state, &cookies)?;
    verify_session_read_access(&app_state, session_id, current_user_id)?;

    let request_id = Uuid::new_v4();
    let (tx, rx) = oneshot::channel();
    app_state
        .session_manager
        .pending_file_downloads
        .insert(request_id, tx);

    let sent = app_state.session_manager.send_to_connected_session(
        &session_id.to_string(),
        ServerToProxy::FileDownloadRequest(FileDownloadRequestFields {
            request_id,
            path: query.path,
            max_bytes: MAX_PULL_BYTES,
        }),
    );

    if !sent {
        app_state
            .session_manager
            .pending_file_downloads
            .remove(&request_id);
        return Err(AppError::ServiceUnavailable(
            "Session proxy is not connected",
        ));
    }

    let response = match tokio::time::timeout(PULL_TIMEOUT, rx).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => return Err(AppError::BadGateway("File download response was dropped")),
        Err(_) => {
            app_state
                .session_manager
                .pending_file_downloads
                .remove(&request_id);
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
        .filter(|s| !s.trim().is_empty())
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
    let mut conn = app_state.db_pool.get()?;
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
}

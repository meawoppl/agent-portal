//! WebSocket handler for `/ws/session/upload`.
//!
//! Receives chunked image uploads from the proxy. The first message is
//! `Start` (JSON text frame) carrying an auth token; subsequent `Chunk`
//! frames are binary; `Complete` (JSON text) finalizes and the server
//! responds with `Ack { image_url }`. The image is then served at the
//! returned URL via the existing `/api/images/{id}` route.

use crate::AppState;
use axum::extract::ws::WebSocket;
use shared::{ImageUploadClientMsg, ImageUploadEndpoint, ImageUploadServerMsg};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

const MAX_TOTAL_CHUNK_BYTES_LIMIT: u64 = 1024 * 1024 * 1024; // 1 GiB hard ceiling

pub async fn handle_image_upload_socket(socket: WebSocket, app_state: Arc<AppState>) {
    let conn = ws_bridge::server::into_connection::<ImageUploadEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();

    // The upload an authenticated session is currently working on.
    // Set on the first valid Start message and used to abort on disconnect.
    let mut active_upload: Option<Uuid> = None;

    while let Some(result) = ws_receiver.recv().await {
        let msg = match result {
            Ok(m) => m,
            Err(e) => {
                warn!("Image upload WS decode error: {}", e);
                continue;
            }
        };

        match msg {
            ImageUploadClientMsg::Start {
                upload_id,
                session_id,
                auth_token,
                media_type,
                total_bytes,
                file_path,
            } => {
                // Authorize and resolve the inserting user in one go — the
                // upload entry needs a `user_id` for the `serve_image` auth
                // check (#786), and the proxy token already carries it.
                let user_id = match authorize_upload(&app_state, &auth_token) {
                    Some(uid) => uid,
                    None => {
                        let _ = ws_sender
                            .send(ImageUploadServerMsg::Failed {
                                upload_id,
                                reason: "authentication failed".into(),
                            })
                            .await;
                        break;
                    }
                };

                let max_bytes = (app_state.max_image_mb as u64)
                    .saturating_mul(1024 * 1024)
                    .min(MAX_TOTAL_CHUNK_BYTES_LIMIT);

                match app_state.image_store.start_upload(
                    upload_id,
                    &media_type,
                    total_bytes,
                    max_bytes,
                    user_id,
                    Some(session_id),
                ) {
                    Ok(()) => {
                        info!(
                            "Image upload started: id={} session={} media={} size={} path={:?}",
                            upload_id, session_id, media_type, total_bytes, file_path
                        );
                        active_upload = Some(upload_id);
                    }
                    Err(e) => {
                        warn!("Image upload rejected at Start: id={} {}", upload_id, e);
                        let _ = ws_sender
                            .send(ImageUploadServerMsg::Failed {
                                upload_id,
                                reason: e.to_string(),
                            })
                            .await;
                    }
                }
            }
            ImageUploadClientMsg::Chunk {
                upload_id,
                offset,
                data,
            } => {
                if active_upload != Some(upload_id) {
                    warn!(
                        "Chunk for upload {} without a matching Start; ignoring",
                        upload_id
                    );
                    let _ = ws_sender
                        .send(ImageUploadServerMsg::Failed {
                            upload_id,
                            reason: "chunk received before Start".into(),
                        })
                        .await;
                    continue;
                }
                if let Err(e) = app_state.image_store.append_chunk(upload_id, offset, &data) {
                    warn!("Image upload chunk rejected: id={} {}", upload_id, e);
                    app_state.image_store.abort_upload(upload_id);
                    active_upload = None;
                    let _ = ws_sender
                        .send(ImageUploadServerMsg::Failed {
                            upload_id,
                            reason: e.to_string(),
                        })
                        .await;
                }
            }
            ImageUploadClientMsg::Complete { upload_id } => {
                if active_upload != Some(upload_id) {
                    let _ = ws_sender
                        .send(ImageUploadServerMsg::Failed {
                            upload_id,
                            reason: "complete for unknown upload".into(),
                        })
                        .await;
                    continue;
                }
                match app_state.image_store.finalize_upload(upload_id) {
                    Ok(image_id) => {
                        let image_url = format!("/api/images/{}", image_id);
                        info!("Image upload finalized: id={} -> {}", upload_id, image_url);
                        active_upload = None;
                        let _ = ws_sender
                            .send(ImageUploadServerMsg::Ack {
                                upload_id,
                                image_url,
                            })
                            .await;
                    }
                    Err(e) => {
                        warn!("Image upload finalize failed: id={} {}", upload_id, e);
                        active_upload = None;
                        let _ = ws_sender
                            .send(ImageUploadServerMsg::Failed {
                                upload_id,
                                reason: e.to_string(),
                            })
                            .await;
                    }
                }
            }
        }
    }

    // Clean up any in-progress upload if the socket dropped before Complete.
    if let Some(upload_id) = active_upload {
        app_state.image_store.abort_upload(upload_id);
    }
}

/// Verify the proxy auth token and return the user_id it belongs to.
/// `None` on any failure (bad token, banned user, DB error) — caller treats
/// this as "reject the upload".
fn authorize_upload(app_state: &AppState, auth_token: &str) -> Option<Uuid> {
    let mut conn = match app_state.db_pool.get() {
        Ok(c) => c,
        Err(e) => {
            warn!("Image upload auth: failed to get DB conn: {}", e);
            return None;
        }
    };
    crate::handlers::proxy_tokens::verify_and_get_user(app_state, &mut conn, auth_token)
        .ok()
        .map(|(uid, _email)| uid)
}

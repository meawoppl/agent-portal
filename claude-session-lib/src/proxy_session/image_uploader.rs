//! Chunked image upload client.
//!
//! Opens a short-lived connection to `/ws/session/upload`, streams the raw
//! image bytes in binary chunks, and returns the `/api/images/{id}` URL
//! that the backend assigned. Used by the output forwarder when an image
//! is too large to inline as base64 in a single WebSocket frame.

use anyhow::{anyhow, Context, Result};
use shared::{ImageUploadClientMsg, ImageUploadEndpoint, ImageUploadServerMsg};
use std::time::Duration;
use tracing::{debug, warn};
use uuid::Uuid;

/// Maximum bytes per binary chunk frame. Keeps each frame well below the
/// tokio-tungstenite default 64 MB cap with plenty of headroom for the
/// 24-byte chunk header and protocol overhead.
const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Hard timeout for finalize ACK. Should be quick once the last chunk lands.
const ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Send an image to the backend over a fresh upload connection.
///
/// Returns the URL the backend assigned (e.g. `/api/images/abc-…`) which
/// can be substituted into a portal message in place of inline base64.
pub async fn upload_image(
    backend_url: &str,
    session_id: Uuid,
    auth_token: &str,
    media_type: &str,
    file_path: Option<&str>,
    bytes: &[u8],
) -> Result<String> {
    let upload_id = Uuid::new_v4();
    let total_bytes = bytes.len() as u64;
    debug!(
        "Starting chunked image upload: id={} size={} chunks={}",
        upload_id,
        total_bytes,
        bytes.len().div_ceil(CHUNK_SIZE)
    );

    let conn = ws_bridge::native_client::connect::<ImageUploadEndpoint>(backend_url)
        .await
        .context("connect to /ws/session/upload")?;
    let (mut ws_write, mut ws_read) = conn.split();

    let start_msg = ImageUploadClientMsg::Start {
        upload_id,
        session_id,
        auth_token: auth_token.to_string(),
        media_type: media_type.to_string(),
        total_bytes,
        file_path: file_path.map(|s| s.to_string()),
    };
    ws_write.send(start_msg).await.context("send Start frame")?;

    let mut offset: u64 = 0;
    for chunk in bytes.chunks(CHUNK_SIZE) {
        let msg = ImageUploadClientMsg::Chunk {
            upload_id,
            offset,
            data: chunk.to_vec(),
        };
        if let Err(e) = ws_write.send(msg).await {
            return Err(anyhow!("send Chunk frame at offset {}: {}", offset, e));
        }
        offset += chunk.len() as u64;
    }

    ws_write
        .send(ImageUploadClientMsg::Complete { upload_id })
        .await
        .context("send Complete frame")?;

    let response = tokio::time::timeout(ACK_TIMEOUT, ws_read.recv())
        .await
        .map_err(|_| anyhow!("upload ack timeout after {:?}", ACK_TIMEOUT))?;

    match response {
        Some(Ok(ImageUploadServerMsg::Ack {
            upload_id: u,
            image_url,
        })) => {
            if u != upload_id {
                return Err(anyhow!(
                    "upload_id mismatch: expected {}, got {}",
                    upload_id,
                    u
                ));
            }
            debug!("Image upload complete: id={} url={}", upload_id, image_url);
            Ok(image_url)
        }
        Some(Ok(ImageUploadServerMsg::Failed {
            upload_id: _,
            reason,
        })) => {
            warn!("Image upload rejected by backend: {}", reason);
            Err(anyhow!("upload rejected: {}", reason))
        }
        Some(Err(e)) => Err(anyhow!("upload socket decode error: {}", e)),
        None => Err(anyhow!("upload socket closed before ack")),
    }
}

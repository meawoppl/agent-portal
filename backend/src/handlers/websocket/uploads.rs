use super::{SessionId, SessionManager};
use base64::Engine;
use shared::protocol::{MAX_UPLOAD_CHUNK_BYTES, MAX_UPLOAD_TOTAL_CHUNKS};
use shared::ServerToProxy;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

/// Tracks metadata for an in-progress upload so we can validate chunks.
/// `received_indices` is the unique set of chunk indices we've accepted —
/// counting raw arrivals (the old `received_count: u32` field) let a client
/// "complete" an upload by re-sending the same `chunk_index` `total_chunks`
/// times. See #785.
pub(super) struct PendingUpload {
    total_chunks: u32,
    /// Total decoded bytes the client declared up front at `Start`. Used to
    /// detect runaway uploads (running decoded bytes exceeding the declared
    /// total) and to short-circuit the per-server `effective_max_total_bytes`
    /// cap when the client honestly declared a smaller upload.
    total_size: u64,
    /// Per-server byte cap derived from `PORTAL_MAX_IMAGE_MB` at `Start`
    /// time. The binding limit on a single upload's running decoded bytes is
    /// `min(total_size, effective_max_total_bytes)` — declaring a smaller
    /// `total_size` doesn't let you exceed the server cap, and the server cap
    /// doesn't let you exceed an honestly-declared `total_size`.
    effective_max_total_bytes: u64,
    received_indices: HashSet<u32>,
    /// Running total of decoded bytes across all accepted chunks. Compared
    /// against `effective_max_total_bytes` after every chunk to abort the
    /// upload as soon as the cap would be exceeded (rather than only catching
    /// it at finalize time).
    received_bytes: u64,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_file_upload_start(
    session_manager: &SessionManager,
    session_key: &Option<SessionId>,
    pending_uploads: &mut HashMap<String, PendingUpload>,
    upload_id: String,
    filename: String,
    content_type: String,
    total_chunks: u32,
    total_size: u64,
    max_image_mb: u32,
) {
    if total_chunks == 0 || total_chunks > MAX_UPLOAD_TOTAL_CHUNKS {
        warn!(
            "Invalid total_chunks {} for upload {}",
            total_chunks, upload_id
        );
        return;
    }

    // Server-side hard cap on total decoded bytes, derived from
    // `PORTAL_MAX_IMAGE_MB`. Saturating to avoid overflow on absurd configs.
    let effective_max_total_bytes = (max_image_mb as u64).saturating_mul(1024 * 1024);

    if total_size > effective_max_total_bytes {
        warn!(
            "Upload {} declared total_size {} > server cap {} bytes; rejecting",
            upload_id, total_size, effective_max_total_bytes
        );
        return;
    }

    let safe_filename = sanitize_filename(&filename);
    info!(
        "File upload started: {} ({} chunks, {} bytes) upload_id={}",
        safe_filename, total_chunks, total_size, upload_id
    );

    pending_uploads.insert(
        upload_id.clone(),
        PendingUpload {
            total_chunks,
            total_size,
            effective_max_total_bytes,
            received_indices: HashSet::new(),
            received_bytes: 0,
        },
    );

    // Forward start message to proxy
    if let Some(ref key) = session_key {
        let msg = ServerToProxy::FileUploadStart(shared::FileUploadStartFields {
            upload_id,
            filename: safe_filename,
            content_type,
            total_chunks,
            total_size,
        });
        if !session_manager.send_to_session(key, msg) {
            warn!("Session not connected for file upload start");
        }
    }
}

/// Result of validating an incoming chunk against the `PendingUpload` state.
/// Extracted so the test suite can pin the validation contract directly
/// without standing up a `SessionManager`.
#[derive(Debug, PartialEq, Eq)]
enum ChunkOutcome {
    /// Forward this chunk to the proxy.
    Accept,
    /// Forward this chunk and then drop the upload (final unique chunk).
    AcceptAndComplete,
    /// Silently ignore: duplicate index or out-of-range. Don't bump counters.
    Ignore,
    /// Abort the upload: per-chunk cap exceeded, running-total cap exceeded,
    /// declared total exceeded, or invalid base64. Drop the `PendingUpload`
    /// entry so subsequent chunks for the same `upload_id` are rejected.
    Abort(&'static str),
}

/// Pure chunk-validation step: separated from `handle_file_upload_chunk` so
/// the tests can exercise it without a `SessionManager`. Mutates `upload`
/// only on `Accept` / `AcceptAndComplete`.
fn validate_and_record_chunk(
    upload: &mut PendingUpload,
    chunk_index: u32,
    decoded_len: usize,
) -> ChunkOutcome {
    if chunk_index >= upload.total_chunks {
        return ChunkOutcome::Ignore;
    }
    if upload.received_indices.contains(&chunk_index) {
        return ChunkOutcome::Ignore;
    }
    if decoded_len > MAX_UPLOAD_CHUNK_BYTES {
        return ChunkOutcome::Abort("per-chunk byte cap exceeded");
    }

    // Running-total check: compare against the binding cap, which is the
    // smaller of (declared total_size, server PORTAL_MAX_IMAGE_MB cap).
    // Honest clients can't exceed what they declared; dishonest clients
    // (declared small, sending big) can't exceed the server's hard cap.
    let binding_cap = upload.total_size.min(upload.effective_max_total_bytes);
    let new_total = upload.received_bytes.saturating_add(decoded_len as u64);
    if new_total > binding_cap {
        return ChunkOutcome::Abort("running-total byte cap exceeded");
    }

    upload.received_indices.insert(chunk_index);
    upload.received_bytes = new_total;

    if upload.received_indices.len() as u32 >= upload.total_chunks {
        ChunkOutcome::AcceptAndComplete
    } else {
        ChunkOutcome::Accept
    }
}

pub(super) fn handle_file_upload_chunk(
    session_manager: &SessionManager,
    session_key: &Option<SessionId>,
    pending_uploads: &mut HashMap<String, PendingUpload>,
    upload_id: String,
    chunk_index: u32,
    data: String,
) {
    let Some(upload) = pending_uploads.get_mut(&upload_id) else {
        warn!("Received chunk for unknown upload_id={}", upload_id);
        return;
    };

    // Decode upfront so we can enforce the per-chunk and running-total caps
    // on the *decoded* payload (the wire shape is base64). Aborting here on
    // a decode failure prevents the proxy from seeing garbage chunks the
    // backend already accepted into its byte budget.
    let decoded_len = match base64::engine::general_purpose::STANDARD.decode(data.as_bytes()) {
        Ok(bytes) => bytes.len(),
        Err(_) => {
            warn!(
                "Upload {} chunk {} is not valid base64; aborting",
                upload_id, chunk_index
            );
            pending_uploads.remove(&upload_id);
            return;
        }
    };

    let outcome = validate_and_record_chunk(upload, chunk_index, decoded_len);
    match outcome {
        ChunkOutcome::Ignore => {
            warn!(
                "Ignoring chunk {} for upload {} (duplicate or out-of-range; total={})",
                chunk_index, upload_id, upload.total_chunks
            );
            return;
        }
        ChunkOutcome::Abort(reason) => {
            warn!(
                "Aborting upload {} on chunk {}: {}",
                upload_id, chunk_index, reason
            );
            pending_uploads.remove(&upload_id);
            return;
        }
        ChunkOutcome::Accept | ChunkOutcome::AcceptAndComplete => {}
    }

    // Forward chunk directly to proxy
    if let Some(ref key) = session_key {
        let msg = ServerToProxy::FileUploadChunk(shared::FileUploadChunkFields {
            upload_id: upload_id.clone(),
            chunk_index,
            data,
        });
        if !session_manager.send_to_session(key, msg) {
            warn!("Session not connected for file upload chunk");
        }
    }

    // Clean up tracking when all chunks forwarded
    if outcome == ChunkOutcome::AcceptAndComplete {
        info!(
            "All {} chunks forwarded for upload_id={}",
            upload.total_chunks, upload_id
        );
        pending_uploads.remove(&upload_id);
    }
}

fn sanitize_filename(name: &str) -> String {
    let base = name
        .rsplit('/')
        .next()
        .or_else(|| name.rsplit('\\').next())
        .unwrap_or(name);

    let clean: String = base
        .chars()
        .filter(|c| *c != '/' && *c != '\\' && *c != '\0')
        .collect();

    if clean.is_empty() || clean == "." || clean == ".." {
        "uploaded_file".to_string()
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    //! These tests pin the chunk-validation contract described in #785: the
    //! prior implementation bumped `received_count` on every arrival (so a
    //! client sending the same chunk index N times would "complete" the
    //! upload with a single chunk) and enforced only a chunk-count budget
    //! (`MAX_TOTAL_CHUNKS = 51_200`) — never the per-chunk decoded byte size
    //! — so a client sending 10 MB chunks got `10 * 51_200 = 512 GB` of
    //! effective budget. The new validation rules:
    //!
    //!   * duplicate `chunk_index` → ignored, no counter bump
    //!   * `chunk_index >= total_chunks` → ignored
    //!   * decoded chunk > `MAX_UPLOAD_CHUNK_BYTES` → abort
    //!   * running decoded bytes > `min(total_size, server cap)` → abort
    //!   * complete only when **distinct** indices == `total_chunks`
    //!
    //! Verified directly against `validate_and_record_chunk` (pure) plus one
    //! end-to-end test that drives `handle_file_upload_chunk` with a real
    //! `SessionManager` and asserts the forwarded `ServerToProxy` shape.
    use super::*;
    use base64::Engine;
    use shared::ServerToProxy;
    use tokio::sync::mpsc;

    fn upload_with(total_chunks: u32, total_size: u64, server_cap_bytes: u64) -> PendingUpload {
        PendingUpload {
            total_chunks,
            total_size,
            effective_max_total_bytes: server_cap_bytes,
            received_indices: HashSet::new(),
            received_bytes: 0,
        }
    }

    #[test]
    fn duplicate_chunk_index_is_ignored_and_does_not_complete() {
        // Two chunks expected; client sends index 0 twice. The upload must
        // NOT complete on the second arrival — the prior `received_count +=
        // 1` path would incorrectly hit `received_count >= total_chunks`
        // after two identical chunks.
        let mut upload = upload_with(2, 100, 100);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 10),
            ChunkOutcome::Accept
        );
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 10),
            ChunkOutcome::Ignore
        );
        assert_eq!(upload.received_indices.len(), 1);
        assert_eq!(upload.received_bytes, 10);
    }

    #[test]
    fn out_of_range_chunk_index_is_ignored() {
        let mut upload = upload_with(2, 100, 100);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 5, 10),
            ChunkOutcome::Ignore
        );
        assert_eq!(upload.received_indices.len(), 0);
        assert_eq!(upload.received_bytes, 0);
    }

    #[test]
    fn per_chunk_byte_cap_aborts_upload() {
        let mut upload = upload_with(2, u64::MAX, u64::MAX);
        let outcome = validate_and_record_chunk(&mut upload, 0, MAX_UPLOAD_CHUNK_BYTES + 1);
        assert!(
            matches!(outcome, ChunkOutcome::Abort(_)),
            "expected Abort, got {:?}",
            outcome
        );
        // State must not have been mutated on an abort path.
        assert_eq!(upload.received_indices.len(), 0);
        assert_eq!(upload.received_bytes, 0);
    }

    #[test]
    fn running_total_byte_cap_aborts_upload() {
        // Server cap is 100 bytes total; two 60-byte chunks would put us at
        // 120 > 100. First accepts, second aborts.
        let mut upload = upload_with(2, 200, 100);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 60),
            ChunkOutcome::Accept
        );
        let outcome = validate_and_record_chunk(&mut upload, 1, 60);
        assert!(matches!(outcome, ChunkOutcome::Abort(_)));
        // The aborted chunk's bytes must not have been credited.
        assert_eq!(upload.received_bytes, 60);
        assert_eq!(upload.received_indices.len(), 1);
    }

    #[test]
    fn distinct_chunks_in_any_order_complete_the_upload() {
        let mut upload = upload_with(3, 30, 30);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 2, 10),
            ChunkOutcome::Accept
        );
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 10),
            ChunkOutcome::Accept
        );
        assert_eq!(
            validate_and_record_chunk(&mut upload, 1, 10),
            ChunkOutcome::AcceptAndComplete
        );
        assert_eq!(upload.received_indices.len(), 3);
        assert_eq!(upload.received_bytes, 30);
    }

    #[test]
    fn declared_total_size_is_the_binding_cap_when_smaller_than_server_cap() {
        // Client declared 50 bytes, server cap is 1 MiB. A 60-byte chunk
        // must abort even though the server cap is huge.
        let mut upload = upload_with(2, 50, 1024 * 1024);
        let outcome = validate_and_record_chunk(&mut upload, 0, 60);
        assert!(matches!(outcome, ChunkOutcome::Abort(_)));
    }

    #[tokio::test]
    async fn handle_chunk_e2e_forwards_distinct_chunks_and_clears_pending() {
        // End-to-end: drive `handle_file_upload_chunk` against a real
        // SessionManager and assert (a) every distinct chunk gets forwarded
        // to the proxy receiver, (b) duplicate-index chunks do NOT get
        // forwarded, and (c) `pending_uploads` is cleared once the unique
        // chunk count hits `total_chunks`.
        let session_manager = SessionManager::new();
        let session_key: SessionId = "test-session".to_string();
        let (proxy_tx, mut proxy_rx) = mpsc::unbounded_channel();
        session_manager.register_session(session_key.clone(), proxy_tx);

        let mut pending_uploads: HashMap<String, PendingUpload> = HashMap::new();
        let upload_id = "u1".to_string();
        pending_uploads.insert(upload_id.clone(), upload_with(3, 30, 1024 * 1024));

        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 10]);

        // Three distinct chunks, with one duplicate sandwiched in.
        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            0,
            b64.clone(),
        );
        // Duplicate — must be ignored, must NOT complete the upload.
        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            0,
            b64.clone(),
        );
        assert!(
            pending_uploads.contains_key(&upload_id),
            "duplicate chunk completed upload"
        );

        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            1,
            b64.clone(),
        );
        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            2,
            b64.clone(),
        );
        assert!(
            !pending_uploads.contains_key(&upload_id),
            "upload not cleared after completion"
        );

        // Exactly three chunks must have been forwarded to the proxy (the
        // duplicate was dropped). Indices must match what we sent.
        let mut forwarded_indices = Vec::new();
        while let Ok(msg) = proxy_rx.try_recv() {
            match msg {
                ServerToProxy::FileUploadChunk(f) => forwarded_indices.push(f.chunk_index),
                other => panic!("unexpected forwarded message: {:?}", other),
            }
        }
        forwarded_indices.sort_unstable();
        assert_eq!(forwarded_indices, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn handle_chunk_e2e_oversize_chunk_aborts_and_drops_pending() {
        // A chunk decoding to > MAX_UPLOAD_CHUNK_BYTES must abort the upload and
        // remove the pending entry, so subsequent chunks for the same
        // upload_id are silently dropped (the unknown-upload_id path).
        let session_manager = SessionManager::new();
        let session_key: SessionId = "test-session-2".to_string();
        let (proxy_tx, mut proxy_rx) = mpsc::unbounded_channel();
        session_manager.register_session(session_key.clone(), proxy_tx);

        let mut pending_uploads: HashMap<String, PendingUpload> = HashMap::new();
        let upload_id = "u2".to_string();
        pending_uploads.insert(upload_id.clone(), upload_with(2, u64::MAX, u64::MAX));

        let oversize = vec![0u8; MAX_UPLOAD_CHUNK_BYTES + 1];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&oversize);

        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            0,
            b64,
        );

        assert!(
            !pending_uploads.contains_key(&upload_id),
            "oversize chunk did not abort"
        );
        // The oversize chunk must NOT have been forwarded.
        assert!(
            proxy_rx.try_recv().is_err(),
            "aborted chunk was forwarded anyway"
        );
    }
}

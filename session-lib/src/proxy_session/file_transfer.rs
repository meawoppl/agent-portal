//! Connection-scoped uploads/downloads. Only completed uploads are renamed
//! to their destination; partial files are owned until cleanup succeeds.

use super::{
    ws_reader::{FileDownloadEvent, FileUploadEvent},
    SharedWsWrite,
};
use shared::{fmt::truncate_str as truncate, ProxyToServer};
use std::{collections::HashMap, time::Instant};
use tracing::{error, info, warn};

/// Tracks state for a file being received in chunks
pub struct FileReceiveState {
    pub filename: String,
    pub total_chunks: u32,
    pub total_size: u64,
    pub received_chunks: u32,
    pub received_bytes: u64,
    pub file_handle: Option<tokio::fs::File>,
    pub start_time: std::time::Instant,
    pub last_log_percent: u32,
    /// Bytes are written here and renamed to `filename` only on completion,
    /// so a consumer (the agent) can never read a truncated file (#939).
    pub temp_path: std::path::PathBuf,
    pub final_path: std::path::PathBuf,
    pub disposition: shared::FileUploadDisposition,
    partial: PartialFile,
}

pub struct FileTransferBridge {
    working_directory: String,
    ws_write: SharedWsWrite,
    active_uploads: HashMap<String, FileReceiveState>,
}

impl FileTransferBridge {
    pub fn new(working_directory: String, ws_write: SharedWsWrite) -> Self {
        Self {
            working_directory,
            ws_write,
            active_uploads: HashMap::new(),
        }
    }

    pub async fn handle_upload(&mut self, event: FileUploadEvent) {
        handle_file_upload(event, self).await;
    }

    pub async fn handle_download(&mut self, event: FileDownloadEvent) {
        handle_file_download(event, self).await;
    }

    /// Normal teardown awaits cleanup. Keep each entry owned across the await,
    /// so cancellation still leaves it for the Drop fallback.
    pub async fn shutdown(&mut self) {
        for upload in self.active_uploads.values_mut() {
            drop(upload.file_handle.take());
            match tokio::fs::remove_file(&upload.temp_path).await {
                Ok(()) => upload.partial.disarm(),
                Err(error) => report_cleanup_error(&upload.temp_path, error),
            }
        }
        self.active_uploads.clear();
    }
}

fn report_cleanup_error(path: &std::path::Path, error: std::io::Error) {
    if error.kind() != std::io::ErrorKind::NotFound {
        warn!(
            "Failed to remove partial upload {}: {}",
            path.display(),
            error
        );
    }
}

/// Owns only the temporary pathname, never its committed destination. A
/// cancelled open's blocking worker also returns this guard, so dropping its
/// unobserved result cannot strand a file created after connection teardown.
struct PartialFile(Option<std::path::PathBuf>);

impl PartialFile {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        let Some(path) = self.0.take() else {
            return;
        };
        // Cancellation and runtime teardown cannot await cleanup. Keep any
        // blocking filesystem work off the reactor and do not require Tokio
        // to remain alive. Process termination is still best effort.
        if let Err(error) = std::thread::Builder::new()
            .name("portal-upload-cleanup".into())
            .spawn(move || {
                if let Err(error) = std::fs::remove_file(&path) {
                    report_cleanup_error(&path, error);
                }
            })
        {
            warn!("Failed to start partial upload cleanup: {}", error);
        }
    }
}

fn create_partial(path: std::path::PathBuf) -> std::io::Result<(std::fs::File, PartialFile)> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    Ok((file, PartialFile(Some(path))))
}

async fn open_partial(path: std::path::PathBuf) -> std::io::Result<(tokio::fs::File, PartialFile)> {
    let (file, guard) = tokio::task::spawn_blocking(move || create_partial(path))
        .await
        .map_err(std::io::Error::other)??;
    Ok((tokio::fs::File::from_std(file), guard))
}

/// Report an upload's terminal outcome to the backend (relayed to the web
/// client, which gates the prompt referencing the file on it — #939).
async fn send_upload_result(
    state: &FileTransferBridge,
    upload_id: String,
    success: bool,
    error: Option<String>,
    path: Option<String>,
) {
    let mut ws = state.ws_write.lock().await;
    if let Err(e) = ws
        .send(ProxyToServer::FileUploadResult(
            shared::FileUploadResultFields {
                upload_id,
                success,
                error,
                path,
            },
        ))
        .await
    {
        error!("Failed to send file upload result: {}", e);
    }
}

/// Abort an in-flight upload: drop its state, best-effort delete the
/// partial temp file, and report the failure.
async fn fail_upload(state: &mut FileTransferBridge, upload_id: String, reason: String) {
    if let Some(recv_state) = state.active_uploads.get_mut(&upload_id) {
        drop(recv_state.file_handle.take());
        match tokio::fs::remove_file(&recv_state.temp_path).await {
            Ok(()) => recv_state.partial.disarm(),
            Err(error) => report_cleanup_error(&recv_state.temp_path, error),
        }
    }
    state.active_uploads.remove(&upload_id);
    error!("[upload {}] Failed: {}", upload_id_log(&upload_id), reason);
    send_upload_result(state, upload_id, false, Some(reason), None).await;
}

/// Average upload receive rate in KB/s for the progress and completion logs.
///
/// Zero elapsed (a same-instant completion, reachable in tests) reports 0
/// instead of dividing by zero. Single home for the rate math so the
/// milestone and completion logs cannot drift.
fn upload_rate_kbps(received_bytes: u64, elapsed_s: f64) -> f64 {
    if elapsed_s > 0.0 {
        received_bytes as f64 / elapsed_s / 1024.0
    } else {
        0.0
    }
}

/// Short upload id for the `[upload ...]` log lines. Single home for the
/// width so the failure, duplicate-start, milestone, and completion logs
/// cannot drift.
fn upload_id_log(upload_id: &str) -> &str {
    truncate(upload_id, 8)
}

/// 10%-bucketed upload progress for the milestone log. An unknown total
/// size (nothing to measure against) reports 100. Single home for the
/// bucket math so it stays testable next to `upload_rate_kbps`.
fn upload_milestone_percent(received_bytes: u64, total_size: u64) -> u32 {
    if total_size == 0 {
        return 100;
    }
    let percent = ((received_bytes as f64 / total_size as f64) * 100.0) as u32;
    (percent / 10) * 10
}

/// Handle a file upload event (start or chunk)
async fn handle_file_upload(upload_event: FileUploadEvent, state: &mut FileTransferBridge) {
    match upload_event {
        FileUploadEvent::Start {
            upload_id,
            filename,
            total_chunks,
            total_size,
            disposition,
        } => {
            // Sanitize filename
            let safe_name: String = filename
                .rsplit('/')
                .next()
                .or_else(|| filename.rsplit('\\').next())
                .unwrap_or(&filename)
                .chars()
                .filter(|c| *c != '/' && *c != '\\' && *c != '\0')
                .collect();
            let safe_name = if safe_name.is_empty() || safe_name == "." || safe_name == ".." {
                "uploaded_file".to_string()
            } else {
                safe_name
            };

            // Retain ownership through unlink: cancellation during cleanup
            // must still leave the old partial for the bridge's Drop guard.
            if let Some(old) = state.active_uploads.get_mut(&upload_id) {
                warn!(
                    "[upload {}] Duplicate start; discarding previous partial",
                    upload_id_log(&upload_id)
                );
                drop(old.file_handle.take());
                match tokio::fs::remove_file(&old.temp_path).await {
                    Ok(()) => old.partial.disarm(),
                    Err(error) => report_cleanup_error(&old.temp_path, error),
                }
            }

            state.active_uploads.remove(&upload_id);

            // Write to a hidden temp path; renamed to the real name only on
            // completion so the agent can never read a truncated file.
            let id = uuid::Uuid::new_v4();
            let (temp_path, final_path) = match disposition {
                shared::FileUploadDisposition::Workspace => {
                    let final_path =
                        std::path::Path::new(&state.working_directory).join(&safe_name);
                    let temp_name = format!(".{safe_name}.{id}.upload");
                    (
                        std::path::Path::new(&state.working_directory).join(temp_name),
                        final_path,
                    )
                }
                shared::FileUploadDisposition::SecretDrop => {
                    let root = std::env::var_os("XDG_RUNTIME_DIR")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(std::env::temp_dir);
                    (
                        root.join(format!(".portal-drop-{id}.upload")),
                        root.join(format!("portal-drop-{id}")),
                    )
                }
            };
            match open_partial(temp_path.clone()).await {
                Ok((fh, partial)) => {
                    state.active_uploads.insert(
                        upload_id,
                        FileReceiveState {
                            filename: safe_name,
                            total_chunks,
                            total_size,
                            received_chunks: 0,
                            received_bytes: 0,
                            file_handle: Some(fh),
                            start_time: Instant::now(),
                            last_log_percent: 0,
                            temp_path,
                            final_path,
                            disposition,
                            partial,
                        },
                    );
                }
                Err(e) => {
                    let reason = format!("failed to create {}: {}", temp_path.display(), e);
                    fail_upload(state, upload_id, reason).await;
                }
            }
        }
        FileUploadEvent::Chunk { upload_id, data } => {
            use base64::Engine;
            use tokio::io::AsyncWriteExt;

            let upload_id_short = upload_id_log(&upload_id);
            let Some(recv_state) = state.active_uploads.get_mut(&upload_id) else {
                // Post-abort stragglers land here; the failure was already
                // reported when the upload state was dropped.
                warn!("[upload {}] Chunk for unknown upload", upload_id_short);
                return;
            };

            let decoded = match base64::engine::general_purpose::STANDARD.decode(&data) {
                Ok(b) => b,
                Err(e) => {
                    let reason = format!("base64 decode error: {e}");
                    fail_upload(state, upload_id, reason).await;
                    return;
                }
            };

            if let Some(ref mut fh) = recv_state.file_handle {
                if let Err(e) = fh.write_all(&decoded).await {
                    let reason = format!("write error: {e}");
                    fail_upload(state, upload_id, reason).await;
                    return;
                }
            }

            recv_state.received_chunks += 1;
            recv_state.received_bytes += decoded.len() as u64;

            // Log every 10% milestone
            let log_threshold =
                upload_milestone_percent(recv_state.received_bytes, recv_state.total_size);
            if log_threshold > recv_state.last_log_percent {
                let elapsed = recv_state.start_time.elapsed().as_secs_f64();
                let rate_kb = upload_rate_kbps(recv_state.received_bytes, elapsed);
                info!(
                    "[upload {}] {} - {}% ({}/{} bytes) - {:.1} KB/s",
                    upload_id_short,
                    recv_state.filename,
                    log_threshold.min(100),
                    recv_state.received_bytes,
                    recv_state.total_size,
                    rate_kb
                );
                recv_state.last_log_percent = log_threshold;
            }

            // Check if complete
            if recv_state.received_chunks >= recv_state.total_chunks {
                use tokio::io::AsyncWriteExt;

                let elapsed = recv_state.start_time.elapsed().as_secs_f64();
                let rate_kb = upload_rate_kbps(recv_state.received_bytes, elapsed);

                // Flush + close, then commit: rename the temp file to its
                // real name. Only a successful rename counts as delivered.
                if let Some(mut fh) = recv_state.file_handle.take() {
                    if let Err(e) = fh.flush().await {
                        let reason = format!("flush error: {e}");
                        fail_upload(state, upload_id, reason).await;
                        return;
                    }
                }

                let filename = recv_state.filename.clone();
                let received_bytes = recv_state.received_bytes;
                // Every chunk arrived, but an empty or short file must not be
                // committed and reported as delivered.
                if received_bytes == 0 || received_bytes != recv_state.total_size {
                    let reason = format!(
                        "received {received_bytes} of {} declared bytes",
                        recv_state.total_size
                    );
                    fail_upload(state, upload_id, reason).await;
                    return;
                }
                let final_path = recv_state.final_path.clone();
                let temp_path = recv_state.temp_path.clone();
                if let Err(e) = tokio::fs::rename(&temp_path, &final_path).await {
                    let reason = format!("rename to {} failed: {e}", final_path.display());
                    fail_upload(state, upload_id, reason).await;
                    return;
                }

                recv_state.partial.disarm();
                info!(
                    "[upload {}] Complete: {} ({} bytes in {:.1}s, avg {:.1} KB/s)",
                    upload_id_short, filename, received_bytes, elapsed, rate_kb
                );
                let is_secret_drop =
                    recv_state.disposition == shared::FileUploadDisposition::SecretDrop;
                let secret_path = is_secret_drop.then(|| final_path.display().to_string());
                state.active_uploads.remove(&upload_id);
                send_upload_result(state, upload_id, true, None, secret_path).await;
                if is_secret_drop {
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
                        let _ = tokio::fs::remove_file(final_path).await;
                    });
                }
            }
        }
    }
}

async fn handle_file_download(download_event: FileDownloadEvent, state: &mut FileTransferBridge) {
    let FileDownloadEvent::Request(request) = download_event;
    let response = read_download_file(&state.working_directory, &request).await;
    let mut ws = state.ws_write.lock().await;
    if let Err(e) = ws.send(ProxyToServer::FileDownloadResponse(response)).await {
        error!("Failed to send file download response: {}", e);
    }
}

async fn read_download_file(
    working_directory: &str,
    request: &shared::FileDownloadRequestFields,
) -> shared::FileDownloadResponseFields {
    use base64::Engine;
    use std::path::{Component, Path};

    let fail = |error: &str| shared::FileDownloadResponseFields {
        request_id: request.request_id,
        success: false,
        filename: None,
        media_type: None,
        size: None,
        data_base64: None,
        error: Some(error.to_string()),
    };

    let requested = Path::new(&request.path);
    if requested.is_absolute()
        || requested.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return fail("invalid relative path");
    }

    let root = match tokio::fs::canonicalize(working_directory).await {
        Ok(path) => path,
        Err(_) => return fail("working directory is unavailable"),
    };
    let target = root.join(requested);
    let canonical = match tokio::fs::canonicalize(&target).await {
        Ok(path) => path,
        Err(_) => return fail("file not found"),
    };
    if !canonical.starts_with(&root) {
        return fail("path escapes working directory");
    }

    let metadata = match tokio::fs::metadata(&canonical).await {
        Ok(metadata) => metadata,
        Err(_) => return fail("file not found"),
    };
    if !metadata.is_file() {
        return fail("path is not a file");
    }
    if metadata.len() > request.max_bytes {
        return fail("file exceeds size limit");
    }

    let bytes = match tokio::fs::read(&canonical).await {
        Ok(bytes) => bytes,
        Err(_) => return fail("failed to read file"),
    };
    if bytes.len() as u64 > request.max_bytes {
        return fail("file exceeds size limit");
    }

    let filename = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .to_string();

    shared::FileDownloadResponseFields {
        request_id: request.request_id,
        success: true,
        filename: Some(filename),
        media_type: Some("application/octet-stream".to_string()),
        size: Some(bytes.len() as u64),
        data_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    fn download_request(path: &str, max_bytes: u64) -> shared::FileDownloadRequestFields {
        shared::FileDownloadRequestFields {
            request_id: Uuid::nil(),
            path: path.to_string(),
            max_bytes,
        }
    }

    #[tokio::test]
    async fn read_download_file_returns_valid_file() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("hello.txt"), b"hi there")
            .await
            .unwrap();

        let resp = read_download_file(
            dir.path().to_str().unwrap(),
            &download_request("hello.txt", 1024),
        )
        .await;

        assert!(resp.success);
        assert_eq!(resp.filename.as_deref(), Some("hello.txt"));
        assert_eq!(resp.size, Some(8));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(resp.data_base64.unwrap())
            .unwrap();
        assert_eq!(decoded, b"hi there");
    }

    #[tokio::test]
    async fn read_download_file_rejects_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let resp = read_download_file(
            dir.path().to_str().unwrap(),
            &download_request("/etc/passwd", 1024),
        )
        .await;
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("invalid relative path"));
    }

    #[tokio::test]
    async fn read_download_file_rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let resp = read_download_file(
            dir.path().to_str().unwrap(),
            &download_request("../secrets.txt", 1024),
        )
        .await;
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("invalid relative path"));
    }

    #[tokio::test]
    async fn read_download_file_reports_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let resp = read_download_file(
            dir.path().to_str().unwrap(),
            &download_request("nope.txt", 1024),
        )
        .await;
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("file not found"));
    }

    #[tokio::test]
    async fn read_download_file_enforces_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("big.bin"), vec![0u8; 100])
            .await
            .unwrap();
        let resp = read_download_file(
            dir.path().to_str().unwrap(),
            &download_request("big.bin", 10),
        )
        .await;
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("file exceeds size limit"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_download_file_rejects_symlink_escape() {
        // A symlink whose name is a clean relative path but whose target
        // canonicalizes outside the working directory must be refused.
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("secret.txt"), b"top secret")
            .await
            .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            root.path().join("link.txt"),
        )
        .unwrap();

        let resp = read_download_file(
            root.path().to_str().unwrap(),
            &download_request("link.txt", 1024),
        )
        .await;
        assert!(!resp.success);
        assert_eq!(
            resp.error.as_deref(),
            Some("path escapes working directory")
        );
    }

    #[test]
    fn upload_rate_kbps_matches_milestone_math_and_guards_zero_elapsed() {
        assert_eq!(upload_rate_kbps(0, 1.0), 0.0);
        assert_eq!(upload_rate_kbps(1024, 0.0), 0.0);
        assert_eq!(upload_rate_kbps(1024, 1.0), 1.0);
        assert_eq!(upload_rate_kbps(2048, 0.5), 4.0);
    }

    #[test]
    fn upload_id_log_keeps_short_ids_and_truncates_long_ones() {
        assert_eq!(upload_id_log("abc"), "abc");
        assert_eq!(upload_id_log("12345678"), "12345678");
        assert_eq!(upload_id_log("123456789"), "12345678");
    }

    #[test]
    fn upload_milestone_percent_buckets_by_ten() {
        assert_eq!(upload_milestone_percent(0, 0), 100);
        assert_eq!(upload_milestone_percent(0, 1000), 0);
        assert_eq!(upload_milestone_percent(50, 1000), 0);
        assert_eq!(upload_milestone_percent(149, 1000), 10);
        assert_eq!(upload_milestone_percent(999, 1000), 90);
        assert_eq!(upload_milestone_percent(1000, 1000), 100);
    }
    async fn bridge(root: &std::path::Path) -> (FileTransferBridge, tokio::task::JoinHandle<()>) {
        use futures_util::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            while socket.next().await.is_some() {}
        });
        let connection = ws_bridge::native_client::connect::<shared::SessionEndpoint>(&format!(
            "ws://{address}"
        ))
        .await
        .unwrap();
        let (write, _read) = connection.split();
        (
            FileTransferBridge::new(
                root.display().to_string(),
                std::sync::Arc::new(tokio::sync::Mutex::new(write)),
            ),
            server,
        )
    }

    fn start() -> FileUploadEvent {
        FileUploadEvent::Start {
            upload_id: "upload".into(),
            filename: "report.txt".into(),
            total_chunks: 2,
            total_size: 4,
            disposition: shared::FileUploadDisposition::Workspace,
        }
    }

    fn chunk() -> FileUploadEvent {
        FileUploadEvent::Chunk {
            upload_id: "upload".into(),
            data: "aGk=".into(),
        }
    }

    #[tokio::test]
    async fn disconnect_removes_partial_but_never_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::write(root.path().join("report.txt"), b"original")
            .await
            .unwrap();
        let (mut bridge, server) = bridge(root.path()).await;
        bridge.handle_upload(start()).await;
        bridge.handle_upload(chunk()).await;
        let partial = bridge.active_uploads["upload"].temp_path.clone();
        assert!(partial.exists());
        bridge.shutdown().await;
        assert!(!partial.exists());
        assert_eq!(
            tokio::fs::read(root.path().join("report.txt"))
                .await
                .unwrap(),
            b"original"
        );
        server.abort();
    }

    #[tokio::test]
    async fn completed_upload_survives_shutdown_and_drop() {
        let root = tempfile::tempdir().unwrap();
        let (mut bridge, server) = bridge(root.path()).await;
        bridge.handle_upload(start()).await;
        let partial = bridge.active_uploads["upload"].temp_path.clone();
        bridge.handle_upload(chunk()).await;
        bridge.handle_upload(chunk()).await;
        assert!(bridge.active_uploads.is_empty());
        assert!(!partial.exists());
        bridge.shutdown().await;
        drop(bridge);
        assert_eq!(
            tokio::fs::read(root.path().join("report.txt"))
                .await
                .unwrap(),
            b"hihi"
        );
        server.abort();
    }

    #[tokio::test]
    async fn duplicate_start_and_bad_chunk_remove_only_owned_partials() {
        let root = tempfile::tempdir().unwrap();
        let (mut bridge, server) = bridge(root.path()).await;
        bridge.handle_upload(start()).await;
        let old = bridge.active_uploads["upload"].temp_path.clone();
        bridge.handle_upload(start()).await;
        let new = bridge.active_uploads["upload"].temp_path.clone();
        assert_ne!(old, new);
        assert!(!old.exists());
        assert!(new.exists());
        bridge
            .handle_upload(FileUploadEvent::Chunk {
                upload_id: "upload".into(),
                data: "!invalid!".into(),
            })
            .await;
        assert!(!new.exists());
        assert!(bridge.active_uploads.is_empty());
        assert!(!root.path().join("report.txt").exists());
        server.abort();
    }

    #[test]
    fn dropped_bridge_cleans_partial_after_runtime_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (bridge, partial) = runtime.block_on(async {
            let (mut bridge, server) = bridge(root.path()).await;
            bridge.handle_upload(start()).await;
            bridge.handle_upload(chunk()).await;
            let partial = bridge.active_uploads["upload"].temp_path.clone();
            server.abort();
            (bridge, partial)
        });
        drop(runtime);
        drop(bridge);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while partial.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!partial.exists(), "cleanup must not require a live runtime");
        assert!(!root.path().join("report.txt").exists());
    }
    #[tokio::test]
    async fn cancelled_open_worker_removes_file_created_after_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("late.upload");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (created_tx, created_rx) = tokio::sync::oneshot::channel();
        let worker_path = path.clone();
        let worker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let result = create_partial(worker_path);
            created_tx.send(()).unwrap();
            result
        });
        started_rx.await.unwrap();
        // A started blocking operation cannot be cancelled. Its unobserved
        // return value must own cleanup even when the bridge never sees it.
        worker.abort();
        drop(worker);
        release_tx.send(()).unwrap();
        created_rx.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled worker result must remove late-created file");
    }

    #[test]
    fn failed_create_never_claims_an_existing_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-elsewhere.upload");
        std::fs::write(&path, b"keep").unwrap();
        assert!(create_partial(path.clone()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"keep");
    }
}

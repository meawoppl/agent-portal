//! Move generated images through Portal's authenticated, archived media path
//! before serializing the transcript item. Failed uploads retain a bounded
//! local recovery file and an explicit diagnostic, never an inline DB payload.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine;
use codex_codes::{GeneratedImageRef, Notification, ServerMessage, ThreadItemPayload};
use serde::Serialize;
use session_lib::snapshot::{PortalCredentials, SessionConfig};
use shared::codex_media::CodexImageMedia;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_ENCODED_BYTES: usize = MAX_IMAGE_BYTES.div_ceil(3) * 4 + 1024;
const MAX_RECOVERY_FILES: usize = 32;
const MAX_RECOVERY_FILE_BYTES: usize = MAX_ENCODED_BYTES;
const MAX_RECOVERY_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const RECOVERY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Serialize)]
struct GeneratedItemOutput<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    item: GeneratedItem<'a>,
}
#[derive(Serialize)]
struct GeneratedItem<'a> {
    #[serde(flatten)]
    item: &'a codex_codes::protocol::ThreadItem,
    #[serde(rename = "portalMedia")]
    media: CodexImageMedia,
}

pub(crate) struct GeneratedMediaNormalizer {
    session_id: Uuid,
    main_thread_id: Option<String>,
    credentials: Option<PortalCredentials>,
    client: Option<reqwest::Client>,
    recovery_root: PathBuf,
    // Only small references; never image bytes. Cleared at the next turn and
    // capped independently so unexpected lifecycle streams cannot grow it.
    references: HashMap<String, CodexImageMedia>,
}

impl GeneratedMediaNormalizer {
    pub fn new(config: &SessionConfig) -> Self {
        Self {
            session_id: config.session_id,
            main_thread_id: config.codex_thread_id.clone(),
            credentials: config.portal_credentials.clone(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .ok(),
            recovery_root: session_lib::paths::config_dir().join("generated-image-recovery"),
            references: HashMap::new(),
        }
    }

    pub fn set_main_thread(&mut self, thread_id: &str) {
        self.main_thread_id = Some(thread_id.to_string());
    }

    /// `Some` owns the visible output for this item. All other notifications
    /// continue through the ordinary classifier, preserving output ordering.
    pub async fn normalize(&mut self, msg: &mut ServerMessage) -> Option<serde_json::Value> {
        let (kind, item) = match msg {
            ServerMessage::Notification(Notification::TurnStarted(p)) => {
                if self.main_thread_id.as_deref() == Some(p.thread_id.as_str()) {
                    self.references.clear();
                }
                return None;
            }
            ServerMessage::Notification(Notification::ItemStarted(p)) => {
                ("item.started", &mut p.item)
            }
            ServerMessage::Notification(Notification::ItemCompleted(p)) => {
                ("item.completed", &mut p.item)
            }
            _ => return None,
        };
        let ThreadItemPayload::ImageGeneration {
            id,
            image,
            saved_path,
            ..
        } = &mut item.payload
        else {
            return None;
        };
        // Remove the source string before any await or JSON serialization. A
        // failed upload must not fall back to persisting its original bytes.
        let inline = match image {
            GeneratedImageRef::Inline { result } => std::mem::take(result),
            GeneratedImageRef::File { .. } => String::new(),
        };
        let media = if let Some(media) = self.references.get(id) {
            media.clone()
        } else {
            // Empty start notifications and provider-only attachments have no
            // local bytes to upload. Keep their normal metadata card.
            if inline.is_empty() && (saved_path.is_none() || kind == "item.started") {
                return None;
            }
            let path = saved_path.as_ref().map(|path| PathBuf::from(&path.0));
            let media = self.transfer(inline, path.as_deref()).await;
            if self.references.len() >= MAX_RECOVERY_FILES {
                self.references.clear();
            }
            if media.uploaded || kind == "item.completed" {
                self.references.insert(id.clone(), media.clone());
            }
            media
        };
        Some(crate::events::to_raw_output(&GeneratedItemOutput {
            kind,
            item: GeneratedItem { item, media },
        }))
    }

    async fn transfer(&self, inline: String, saved_path: Option<&Path>) -> CodexImageMedia {
        let decoded = decode_image(&inline, saved_path).await;
        let bytes = match decoded {
            Ok(bytes) => bytes,
            Err(error) => {
                let recovery = if saved_path.is_some() {
                    saved_path.map(Path::to_path_buf)
                } else if !inline.is_empty() && inline.len() <= MAX_ENCODED_BYTES {
                    self.save_recovery(inline.as_bytes(), "base64").await.ok()
                } else {
                    None
                };
                let error = if recovery.is_none() {
                    format!("{error} No temporary recovery file is available; use native Codex history.")
                } else {
                    error
                };
                return CodexImageMedia {
                    error: Some(error),
                    recovery_path: recovery.map(|p| p.display().to_string()),
                    ..Default::default()
                };
            }
        };
        drop(inline);
        let content_type = shared::media::sniff_content_type(&bytes).filter(|content_type| {
            shared::media::media_kind(content_type) == Some(shared::media::MediaKind::Image)
        });
        let result = match content_type {
            Some(content_type) => self.upload(&bytes, content_type).await,
            None => Err("Generated data is not a supported image format.".to_string()),
        };
        match result {
            Ok(response) if response.persisted => CodexImageMedia {
                image_id: response.media_id,
                uploaded: true,
                ..Default::default()
            },
            result => {
                let error = match result {
                    Ok(_) => {
                        "Image upload reached the backend but its media message was not persisted."
                            .to_string()
                    }
                    Err(error) => error,
                };
                let extension = match content_type {
                    Some("image/jpeg") => "jpg",
                    Some("image/webp") => "webp",
                    Some("image/gif") => "gif",
                    Some("image/svg+xml") => "svg",
                    Some("image/png") => "png",
                    _ => "bin",
                };
                let recovery = match saved_path {
                    Some(path) => Some(path.to_path_buf()),
                    None => self.save_recovery(&bytes, extension).await.ok(),
                };
                let error = if recovery.is_none() {
                    format!("{error} Local recovery file could not be saved; recover the asset from native Codex history.")
                } else {
                    error
                };
                CodexImageMedia {
                    error: Some(error),
                    recovery_path: recovery.map(|p| p.display().to_string()),
                    ..Default::default()
                }
            }
        }
    }

    async fn upload(
        &self,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<shared::api::ShowMediaResponse, String> {
        let credentials = self
            .credentials
            .as_ref()
            .ok_or("Image upload is unavailable without Portal credentials.")?;
        let client = self
            .client
            .as_ref()
            .ok_or("Image upload client could not be initialized.")?;
        let base = shared::urls::ws_to_http(&credentials.backend_url);
        let url = format!(
            "{}/api/agent/sessions/{}/media",
            base.trim_end_matches('/'),
            self.session_id
        );
        let response = client
            .post(url)
            .bearer_auth(&credentials.token)
            .query(&[("filename", "codex-generated-image")])
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| {
                "Image upload failed or timed out; retry the recovery file with agent-portal show."
                    .to_string()
            })?;
        if !response.status().is_success() {
            return Err(format!(
                "Image upload failed (HTTP {}).",
                response.status().as_u16()
            ));
        }
        response
            .json()
            .await
            .map_err(|_| "Image upload returned an invalid response.".to_string())
    }

    /// Recovery files are private, expire after 24h, and share a host-wide
    /// count and byte budget. The filesystem lock coordinates launcher
    /// processes; a per-session in-memory lock would not enforce that budget.
    async fn save_recovery(&self, bytes: &[u8], extension: &str) -> std::io::Result<PathBuf> {
        if bytes.len() > MAX_RECOVERY_FILE_BYTES {
            return Err(std::io::Error::other("image exceeds recovery file limit"));
        }
        let root = self.recovery_root.clone();
        let session_id = self.session_id;
        let bytes = bytes.to_vec();
        let extension = extension.to_string();
        tokio::task::spawn_blocking(move || {
            save_recovery_file(&root, session_id, &bytes, &extension)
        })
        .await
        .map_err(std::io::Error::other)?
    }
}

fn save_recovery_file(
    root: &Path,
    session_id: Uuid,
    bytes: &[u8],
    extension: &str,
) -> std::io::Result<PathBuf> {
    use std::io::Write;
    if bytes.len() > MAX_RECOVERY_FILE_BYTES {
        return Err(std::io::Error::other("image exceeds recovery file limit"));
    }
    std::fs::create_dir_all(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(root.join(".lock"))?;
    lock.lock()?;
    let mut retained = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name() == ".lock" || !entry.file_type()?.is_file() {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if modified.elapsed().unwrap_or_default() > RECOVERY_TTL {
            std::fs::remove_file(entry.path())?;
        } else {
            retained.push((modified, entry.path(), metadata.len()));
        }
    }
    retained.sort_by_key(|entry| entry.0);
    let mut total = retained
        .iter()
        .fold(0u64, |sum, entry| sum.saturating_add(entry.2));
    let mut count = retained.len();
    for (_, path, len) in retained {
        if count < MAX_RECOVERY_FILES
            && total.saturating_add(bytes.len() as u64) <= MAX_RECOVERY_TOTAL_BYTES
        {
            break;
        }
        std::fs::remove_file(path)?;
        count -= 1;
        total = total.saturating_sub(len);
    }
    let path = root.join(format!("{session_id}-{}.{}", Uuid::new_v4(), extension));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

async fn decode_image(inline: &str, saved_path: Option<&Path>) -> Result<Vec<u8>, String> {
    if !inline.is_empty() {
        if inline.len() > MAX_ENCODED_BYTES {
            return Err("Generated image exceeds the 10 MiB display limit; recover it from the saved file or native Codex history.".to_string());
        }
        let encoded = inline
            .strip_prefix("data:")
            .and_then(|data| data.split_once(";base64,").map(|(_, body)| body))
            .unwrap_or(inline);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                "Generated image contains invalid base64; an encoded recovery file is needed."
                    .to_string()
            })?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err("Generated image exceeds the 10 MiB display limit.".to_string());
        }
        return Ok(bytes);
    }
    let path = saved_path.ok_or("Generated image has no local bytes or saved path.")?;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| "Generated image's saved file could not be opened.".to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "Generated image's saved file could not be read.".to_string())?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("Generated image exceeds the 10 MiB display limit.".to_string());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn png() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\nimage-fixture".to_vec()
    }
    fn image_frame(kind: &str, bytes: &[u8]) -> ServerMessage {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        ServerMessage::from_json_str(&format!(r#"{{"method":"{kind}","params":{{"threadId":"t","turnId":"turn","item":{{"type":"imageGeneration","id":"image-1","status":"completed","result":"{encoded}"}}}}}}"#)).unwrap()
    }
    fn normalizer(root: &Path, credentials: Option<PortalCredentials>) -> GeneratedMediaNormalizer {
        let config = SessionConfig {
            session_id: Uuid::new_v4(),
            portal_credentials: credentials,
            ..Default::default()
        };
        let mut normalizer = GeneratedMediaNormalizer::new(&config);
        normalizer.recovery_root = root.to_path_buf();
        normalizer
    }

    #[tokio::test]
    async fn unavailable_upload_spools_bytes_and_never_persists_base64() {
        let dir = tempfile::tempdir().unwrap();
        let mut normalizer = normalizer(dir.path(), None);
        let bytes = png();
        let mut msg = image_frame("item/completed", &bytes);
        let output = normalizer.normalize(&mut msg).await.unwrap();
        assert_eq!(output["item"]["result"], "");
        assert_eq!(output["item"]["portalMedia"]["uploaded"], false);
        let path = output["item"]["portalMedia"]["recovery_path"]
            .as_str()
            .unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), bytes);
        assert!(!output
            .to_string()
            .contains(&base64::engine::general_purpose::STANDARD.encode(png())));
        assert!(output["item"]["portalMedia"]["error"]
            .as_str()
            .unwrap()
            .contains("credentials"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    async fn media_server(media_id: Uuid, status: &str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let (header_end, content_len) = loop {
                let mut buffer = [0u8; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    assert!(headers.starts_with("post /api/agent/sessions/"));
                    assert!(headers.contains("authorization: bearer test-token"));
                    assert!(headers.contains("content-type: image/png"));
                    let content_len = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    break (end + 4, content_len);
                }
            };
            while request.len() < header_end + content_len {
                let mut buffer = [0u8; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            assert_eq!(&request[header_end..header_end + content_len], &png());
            let response = shared::api::ShowMediaResponse {
                media_id: Some(media_id),
                session_name: "test".to_string(),
                content_type: "image/png".to_string(),
                persisted: true,
            };
            let body = serde_json::to_string(&response).unwrap();
            socket.write_all(format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        (base, task)
    }

    #[tokio::test]
    async fn upload_replaces_image_with_portal_reference_and_deduplicates_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let (base, server) = media_server(id, "200 OK").await;
        let mut normalizer = normalizer(
            dir.path(),
            Some(PortalCredentials {
                backend_url: base,
                token: "test-token".into(),
            }),
        );
        let mut started = image_frame("item/started", &png());
        let output = normalizer.normalize(&mut started).await.unwrap();
        assert_eq!(output["item"]["portalMedia"]["image_id"], id.to_string());
        assert_eq!(output["item"]["portalMedia"]["uploaded"], true);
        assert_eq!(output["item"]["result"], "");
        server.await.unwrap();
        // The server has closed; completion must reuse the reference, not POST again.
        let mut completed = image_frame("item/completed", &png());
        let completed = normalizer.normalize(&mut completed).await.unwrap();
        assert_eq!(completed["item"]["portalMedia"]["image_id"], id.to_string());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn failed_upload_preserves_recoverable_image_and_visible_error() {
        let dir = tempfile::tempdir().unwrap();
        let (base, server) = media_server(Uuid::new_v4(), "503 Service Unavailable").await;
        let mut normalizer = normalizer(
            dir.path(),
            Some(PortalCredentials {
                backend_url: base,
                token: "test-token".into(),
            }),
        );
        let mut msg = image_frame("item/completed", &png());
        let output = normalizer.normalize(&mut msg).await.unwrap();
        server.await.unwrap();
        assert!(output["item"]["portalMedia"]["error"]
            .as_str()
            .unwrap()
            .contains("503"));
        let path = output["item"]["portalMedia"]["recovery_path"]
            .as_str()
            .unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), png());
        assert_eq!(output["item"]["result"], "");
    }

    #[tokio::test]
    async fn provider_only_file_ids_are_not_portal_identifiers() {
        let dir = tempfile::tempdir().unwrap();
        let mut normalizer = normalizer(dir.path(), None);
        let mut msg = ServerMessage::from_json_str(r#"{"method":"item/completed","params":{"threadId":"t","turnId":"turn","item":{"type":"imageGeneration","id":"image-1","status":"completed","fileId":"provider-file"}}}"#).unwrap();
        assert!(normalizer.normalize(&mut msg).await.is_none());
    }

    #[tokio::test]
    async fn recovery_spool_has_a_hard_file_count_cap() {
        let dir = tempfile::tempdir().unwrap();
        let normalizer = normalizer(dir.path(), None);
        for _ in 0..MAX_RECOVERY_FILES + 3 {
            normalizer.save_recovery(b"image", "png").await.unwrap();
        }
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name() != ".lock")
                .count(),
            MAX_RECOVERY_FILES
        );
    }
    #[tokio::test]
    async fn recovery_spool_enforces_total_and_per_file_byte_limits() {
        let dir = tempfile::tempdir().unwrap();
        // Sparse files exercise byte accounting without writing 64 MiB.
        for index in 0..8 {
            let file = std::fs::File::create(dir.path().join(format!("old-{index}.png"))).unwrap();
            file.set_len(8 * 1024 * 1024).unwrap();
        }
        let normalizer = normalizer(dir.path(), None);
        normalizer.save_recovery(b"new image", "png").await.unwrap();
        let total: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= MAX_RECOVERY_TOTAL_BYTES);
        assert!(normalizer
            .save_recovery(&vec![0; MAX_RECOVERY_FILE_BYTES + 1], "png")
            .await
            .is_err());
    }
    #[tokio::test]
    async fn saved_provider_attachment_keeps_native_recovery_path() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("generated.png");
        tokio::fs::write(&image_path, png()).await.unwrap();
        let mut normalizer = normalizer(&dir.path().join("recovery"), None);
        let path = serde_json::to_string(&image_path.to_string_lossy()).unwrap();
        let mut msg = ServerMessage::from_json_str(&format!(r#"{{"method":"item/completed","params":{{"threadId":"t","turnId":"turn","item":{{"type":"imageGeneration","id":"image-1","status":"completed","fileId":"provider-file","savedPath":{path}}}}}}}"#)).unwrap();
        let output = normalizer.normalize(&mut msg).await.unwrap();
        assert_eq!(output["item"]["fileId"], "provider-file");
        assert_eq!(
            output["item"]["portalMedia"]["recovery_path"],
            image_path.to_string_lossy().as_ref()
        );
        assert_eq!(tokio::fs::read(&image_path).await.unwrap(), png());
    }
}

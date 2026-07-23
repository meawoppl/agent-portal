//! Archive store: config parsing plus the local-filesystem and
//! S3-compatible (`object_store`) backends, shared by the backend (writer)
//! and the standalone archive viewer (reader).

use std::path::{Path, PathBuf};

use object_store::ObjectStoreExt as _;
use uuid::Uuid;

use crate::{
    manifest_key, media_key, media_meta_key, parse_transcript_ndjson, transcript_key, zstd_decode,
    zstd_encode, ArchiveMessageLine, ArchivedMediaMeta, SessionArchiveBundle,
    SessionArchiveManifest,
};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Which object store the archive lives in.
#[derive(Debug, Clone)]
pub enum ArchiveBackendConfig {
    Local {
        root: PathBuf,
    },
    /// S3-compatible object storage. Credentials, region, and endpoint
    /// come from the standard `AWS_*` environment variables (validated at
    /// startup when the store is built).
    S3 {
        bucket: String,
        /// Optional key prefix inside the bucket (no trailing slash).
        prefix: Option<String>,
    },
}

/// Validated archive configuration. `None` anywhere upstream means the
/// feature is disabled (the default, including on hosted deployments).
#[derive(Debug, Clone)]
pub struct ArchiveConfig {
    pub backend: ArchiveBackendConfig,
    /// When false, only manifests are archived (metadata/rollup mode);
    /// transcripts stay subject to normal DB retention.
    pub transcripts: bool,
    /// When true (the default whenever the archive is enabled), media shown via
    /// `agent-portal show` is written through to the archive at upload time and
    /// read back through on a served-store miss (#1450 blobs are ephemeral —
    /// TTL/size bounded — so an archived transcript otherwise permanently shows
    /// "media expired"). Gates both the write-through and the read-through.
    pub media: bool,
}

/// Parse archive settings from the environment. Fail-fast: partial or
/// contradictory config is an error at startup, not a silent fallback.
pub fn archive_config_from_env() -> Result<Option<ArchiveConfig>, String> {
    if std::env::var("PORTAL_SESSION_ARCHIVE_COMPRESS").is_ok() {
        return Err(
            "PORTAL_SESSION_ARCHIVE_COMPRESS has been removed; transcripts are \
             always zstd-compressed — unset it"
                .to_string(),
        );
    }
    let backend =
        std::env::var("PORTAL_SESSION_ARCHIVE_BACKEND").unwrap_or_else(|_| "disabled".to_string());
    let backend = match backend.as_str() {
        "disabled" => return Ok(None),
        "local" => {
            let root = std::env::var("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT").map_err(|_| {
                "PORTAL_SESSION_ARCHIVE_BACKEND=local requires \
                 PORTAL_SESSION_ARCHIVE_LOCAL_ROOT to be set"
                    .to_string()
            })?;
            if root.trim().is_empty() {
                return Err("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT must not be empty".to_string());
            }
            ArchiveBackendConfig::Local {
                root: PathBuf::from(root),
            }
        }
        "s3" => {
            let bucket = std::env::var("PORTAL_SESSION_ARCHIVE_S3_BUCKET").map_err(|_| {
                "PORTAL_SESSION_ARCHIVE_BACKEND=s3 requires \
                 PORTAL_SESSION_ARCHIVE_S3_BUCKET to be set"
                    .to_string()
            })?;
            if bucket.trim().is_empty() {
                return Err("PORTAL_SESSION_ARCHIVE_S3_BUCKET must not be empty".to_string());
            }
            let prefix = match std::env::var("PORTAL_SESSION_ARCHIVE_S3_PREFIX") {
                Ok(p) => {
                    let p = p.trim().trim_matches('/').to_string();
                    if p.is_empty() {
                        return Err(
                            "PORTAL_SESSION_ARCHIVE_S3_PREFIX must not be empty (unset it \
                             to archive at the bucket root)"
                                .to_string(),
                        );
                    }
                    Some(p)
                }
                Err(_) => None,
            };
            ArchiveBackendConfig::S3 { bucket, prefix }
        }
        other => {
            return Err(format!(
                "PORTAL_SESSION_ARCHIVE_BACKEND must be `disabled`, `local`, or `s3`, got `{other}`"
            ))
        }
    };
    let transcripts = match std::env::var("PORTAL_SESSION_ARCHIVE_TRANSCRIPTS")
        .unwrap_or_else(|_| "true".to_string())
        .as_str()
    {
        "true" => true,
        "false" => false,
        other => {
            return Err(format!(
                "PORTAL_SESSION_ARCHIVE_TRANSCRIPTS must be `true` or `false`, got `{other}`"
            ))
        }
    };
    let media = match std::env::var("PORTAL_SESSION_ARCHIVE_MEDIA")
        .unwrap_or_else(|_| "true".to_string())
        .as_str()
    {
        "true" => true,
        "false" => false,
        other => {
            return Err(format!(
                "PORTAL_SESSION_ARCHIVE_MEDIA must be `true` or `false`, got `{other}`"
            ))
        }
    };
    Ok(Some(ArchiveConfig {
        backend,
        transcripts,
        media,
    }))
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Typed archive store. An enum (not a trait object) so backends stay a
/// closed, exhaustively-matched set.
pub enum ArchiveStore {
    Local(LocalArchiveStore),
    /// S3-compatible object storage in production; any `ObjectStore`
    /// implementation (e.g. in-memory) in tests.
    Object(ObjectArchiveStore),
}

impl ArchiveStore {
    pub fn from_config(config: &ArchiveConfig) -> Result<Self, String> {
        match &config.backend {
            ArchiveBackendConfig::Local { root } => {
                Ok(Self::Local(LocalArchiveStore { root: root.clone() }))
            }
            ArchiveBackendConfig::S3 { bucket, prefix } => Ok(Self::Object(
                ObjectArchiveStore::s3_from_env(bucket, prefix.clone())?,
            )),
        }
    }

    /// Write a session's archive: transcript first, manifest last — a
    /// manifest's existence implies its transcript object is complete.
    pub fn put_session_archive(&self, bundle: &SessionArchiveBundle) -> std::io::Result<()> {
        match self {
            Self::Local(store) => store.put_session_archive(bundle),
            Self::Object(store) => store.put_session_archive(bundle),
        }
    }

    pub fn get_session_manifest(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> std::io::Result<Option<SessionArchiveManifest>> {
        match self {
            Self::Local(store) => store.get_session_manifest(user_id, session_id),
            Self::Object(store) => store.get_session_manifest(user_id, session_id),
        }
    }

    /// Read an archived transcript's lines, or `None` if no transcript
    /// object exists yet. Used by the merge-on-rearchive path and the viewer.
    pub fn read_transcript_lines(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> std::io::Result<Option<Vec<ArchiveMessageLine>>> {
        let raw = match self.get_object(&transcript_key(user_id, session_id))? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        parse_transcript_ndjson(&zstd_decode(&raw)?).map(Some)
    }

    /// Write a raw object at `key` (overwrites; deterministic keys make this
    /// idempotent).
    pub fn put_object(&self, key: &str, bytes: Vec<u8>) -> std::io::Result<()> {
        match self {
            Self::Local(store) => store.write_atomic(key, &bytes),
            Self::Object(store) => store.put_bytes(key, bytes),
        }
    }

    /// Read a raw object at `key`, or `None` if it does not exist.
    pub fn get_object(&self, key: &str) -> std::io::Result<Option<Vec<u8>>> {
        match self {
            Self::Local(store) => match std::fs::read(store.object_path(key)) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
            Self::Object(store) => store.get_bytes(key),
        }
    }

    /// Write a media blob and its sidecar. Bytes first, sidecar last — so a
    /// present sidecar implies the bytes are complete (mirrors
    /// transcript-then-manifest).
    pub fn put_media(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        meta: &ArchivedMediaMeta,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        self.put_object(
            &media_key(user_id, session_id, meta.media_id),
            bytes.to_vec(),
        )?;
        let meta_json = serde_json::to_vec(meta)?;
        self.put_object(
            &media_meta_key(user_id, session_id, meta.media_id),
            meta_json,
        )
    }

    /// Read a media blob's sidecar, or `None` if it was never written through.
    /// Sidecar presence is the archive's record that the blob's bytes exist.
    pub fn get_media_meta(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        media_id: Uuid,
    ) -> std::io::Result<Option<ArchivedMediaMeta>> {
        match self.get_object(&media_meta_key(user_id, session_id, media_id))? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("corrupt media sidecar for {media_id}: {e}"),
                )
            })?)),
            None => Ok(None),
        }
    }

    /// Read a media blob's raw bytes, or `None` if absent.
    pub fn get_media_bytes(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        media_id: Uuid,
    ) -> std::io::Result<Option<Vec<u8>>> {
        self.get_object(&media_key(user_id, session_id, media_id))
    }

    // -- Read/list surface (viewer-facing; #1288) ---------------------------

    /// List the user ids present in the archive (`v1/users/{uuid}/`).
    /// Non-UUID entries are ignored (defensive against stray files).
    pub fn list_users(&self) -> std::io::Result<Vec<Uuid>> {
        match self {
            Self::Local(store) => store.list_uuid_dirs(Path::new("v1/users")),
            Self::Object(store) => store.list_uuid_prefixes("v1/users"),
        }
    }

    /// List the session ids archived for `user_id`.
    pub fn list_sessions(&self, user_id: Uuid) -> std::io::Result<Vec<Uuid>> {
        let prefix = format!("v1/users/{user_id}/sessions");
        match self {
            Self::Local(store) => store.list_uuid_dirs(Path::new(&prefix)),
            Self::Object(store) => store.list_uuid_prefixes(&prefix),
        }
    }
}

/// S3-compatible (or any `object_store`) backend. Store methods are sync —
/// they run on the blocking pool alongside the DB work — so each call
/// blocks on the async client via the captured runtime handle.
pub struct ObjectArchiveStore {
    store: std::sync::Arc<dyn object_store::ObjectStore>,
    /// Key prefix inside the bucket (no trailing slash).
    prefix: Option<String>,
    handle: tokio::runtime::Handle,
}

impl ObjectArchiveStore {
    /// Build over any `object_store` implementation (e.g. in-memory in
    /// tests). `prefix` is an optional key prefix (no trailing slash);
    /// `handle` is the tokio runtime the sync methods block on.
    pub fn new(
        store: std::sync::Arc<dyn object_store::ObjectStore>,
        prefix: Option<String>,
        handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            store,
            prefix,
            handle,
        }
    }

    /// Build against S3. Bucket/prefix come from portal config; region,
    /// credentials, and custom endpoints come from the standard `AWS_*`
    /// environment variables (`object_store`'s `from_env`).
    fn s3_from_env(bucket: &str, prefix: Option<String>) -> Result<Self, String> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            "archive S3 store must be constructed inside the tokio runtime".to_string()
        })?;
        let s3 = object_store::aws::AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .build()
            .map_err(|e| format!("invalid S3 archive configuration: {e}"))?;
        Ok(Self {
            store: std::sync::Arc::new(s3),
            prefix,
            handle,
        })
    }

    fn object_path(&self, key: &str) -> object_store::path::Path {
        match &self.prefix {
            Some(prefix) => object_store::path::Path::from(format!("{prefix}/{key}")),
            None => object_store::path::Path::from(key),
        }
    }

    fn put_bytes(&self, key: &str, bytes: Vec<u8>) -> std::io::Result<()> {
        let path = self.object_path(key);
        self.handle
            .block_on(self.store.put(&path, bytes.into()))
            .map(|_| ())
            .map_err(std::io::Error::from)
    }

    fn get_bytes(&self, key: &str) -> std::io::Result<Option<Vec<u8>>> {
        let path = self.object_path(key);
        let result = self
            .handle
            .block_on(async { self.store.get(&path).await?.bytes().await });
        match result {
            Ok(bytes) => Ok(Some(bytes.to_vec())),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(std::io::Error::from(e)),
        }
    }

    /// List the immediate child "directories" of `key_prefix` whose final
    /// segment parses as a UUID (delimiter listing → common prefixes).
    fn list_uuid_prefixes(&self, key_prefix: &str) -> std::io::Result<Vec<Uuid>> {
        let path = self.object_path(key_prefix);
        let listing = self
            .handle
            .block_on(self.store.list_with_delimiter(Some(&path)))
            .map_err(std::io::Error::from)?;
        let mut ids: Vec<Uuid> = listing
            .common_prefixes
            .iter()
            .filter_map(|p| p.parts().next_back())
            .filter_map(|part| Uuid::parse_str(part.as_ref()).ok())
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn put_session_archive(&self, bundle: &SessionArchiveBundle) -> std::io::Result<()> {
        let m = &bundle.manifest;
        if let Some(ndjson) = &bundle.transcript_ndjson {
            let body = zstd_encode(ndjson.as_slice())?;
            self.put_bytes(&transcript_key(m.user_id, m.session_id), body)?;
        }
        let manifest_json = serde_json::to_vec_pretty(&bundle.manifest)?;
        self.put_bytes(&manifest_key(m.user_id, m.session_id), manifest_json)
    }

    fn get_session_manifest(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> std::io::Result<Option<SessionArchiveManifest>> {
        match self.get_bytes(&manifest_key(user_id, session_id))? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("corrupt manifest for session {session_id}: {e}"),
                )
            })?)),
            None => Ok(None),
        }
    }
}

/// Local-filesystem backend. Objects are plain files under `root`; writes
/// are atomic (temp file in the destination directory + rename).
pub struct LocalArchiveStore {
    pub(crate) root: PathBuf,
}

impl LocalArchiveStore {
    /// Build over a local filesystem root.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn object_path(&self, key: &str) -> PathBuf {
        // Keys are built exclusively by the key fns in this crate from UUIDs
        // and fixed literals — no user-controlled segments.
        self.root.join(key)
    }

    fn write_atomic(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        let path = self.object_path(key);
        // Keys are built exclusively by this crate's key fns, so a parent and
        // file name always exist; map defensively rather than panicking.
        let (dir, file_name) = match (path.parent(), path.file_name()) {
            (Some(dir), Some(name)) => (dir, name),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("malformed object key: {key}"),
                ))
            }
        };
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{}.tmp", file_name.to_string_lossy()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    }

    /// List the immediate child directories of `prefix` whose names parse as
    /// UUIDs. A missing prefix directory is an empty archive, not an error.
    fn list_uuid_dirs(&self, prefix: &Path) -> std::io::Result<Vec<Uuid>> {
        let dir = self.root.join(prefix);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn put_session_archive(&self, bundle: &SessionArchiveBundle) -> std::io::Result<()> {
        let m = &bundle.manifest;
        if let Some(ndjson) = &bundle.transcript_ndjson {
            let body = zstd_encode(ndjson.as_slice())?;
            self.write_atomic(&transcript_key(m.user_id, m.session_id), &body)?;
        }
        let manifest_json = serde_json::to_vec_pretty(&bundle.manifest)?;
        self.write_atomic(&manifest_key(m.user_id, m.session_id), &manifest_json)
    }

    fn get_session_manifest(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> std::io::Result<Option<SessionArchiveManifest>> {
        let path = self.object_path(&manifest_key(user_id, session_id));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("corrupt manifest at {}: {e}", path.display()),
                )
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Decompress + parse an archived transcript (test/inspection helper).
pub fn read_transcript(
    root: &Path,
    user_id: Uuid,
    session_id: Uuid,
) -> std::io::Result<Vec<ArchiveMessageLine>> {
    let raw = std::fs::read(root.join(transcript_key(user_id, session_id)))?;
    parse_transcript_ndjson(&zstd_decode(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::manifest;

    fn local_store(root: &Path) -> ArchiveStore {
        ArchiveStore::Local(LocalArchiveStore {
            root: root.to_path_buf(),
        })
    }

    #[test]
    fn put_get_roundtrip_and_listing() {
        let dir = tempfile::tempdir().unwrap();
        let store = local_store(dir.path());

        let u1 = Uuid::from_u128(1);
        let u2 = Uuid::from_u128(2);
        let s1 = Uuid::from_u128(10);
        let s2 = Uuid::from_u128(11);

        for (u, s) in [(u1, s1), (u1, s2), (u2, s1)] {
            let bundle = SessionArchiveBundle {
                manifest: manifest(u, s),
                transcript_ndjson: Some(b"{\"id\":\"01890000-0000-7000-8000-000000000001\",\"role\":\"user\",\"created_at\":\"2026-07-11T00:00:00\",\"agent_type\":\"claude\",\"content\":{}}\n".to_vec()),
            };
            store.put_session_archive(&bundle).unwrap();
        }

        assert_eq!(store.list_users().unwrap(), vec![u1, u2]);
        assert_eq!(store.list_sessions(u1).unwrap(), vec![s1, s2]);
        assert_eq!(store.list_sessions(u2).unwrap(), vec![s1]);
        assert_eq!(
            store.list_sessions(Uuid::from_u128(99)).unwrap(),
            Vec::<Uuid>::new()
        );

        let m = store.get_session_manifest(u1, s1).unwrap().unwrap();
        assert_eq!(m.session_id, s1);
        let lines = store.read_transcript_lines(u1, s1).unwrap().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(store
            .get_session_manifest(Uuid::from_u128(99), s1)
            .unwrap()
            .is_none());
    }

    #[test]
    fn media_sidecar_roundtrip_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = local_store(dir.path());
        let (u, s, m) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));

        let meta = ArchivedMediaMeta {
            media_id: m,
            kind: "image".into(),
            content_type: "image/png".into(),
            filename: Some("plot.png".into()),
            bytes: 4,
            uploaded_at: chrono::NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        };
        store.put_media(u, s, &meta, b"png!").unwrap();

        assert_eq!(store.get_media_meta(u, s, m).unwrap().unwrap(), meta);
        assert_eq!(
            store.get_media_bytes(u, s, m).unwrap().unwrap(),
            b"png!".to_vec()
        );
        assert!(store
            .get_media_meta(u, s, Uuid::from_u128(9))
            .unwrap()
            .is_none());
    }

    #[test]
    fn env_config_rejects_partial_and_unknown() {
        // Note: env-var tests mutate process env; keep them in ONE test so
        // they can't race each other under parallel execution.
        std::env::remove_var("PORTAL_SESSION_ARCHIVE_BACKEND");
        assert!(archive_config_from_env().unwrap().is_none());

        std::env::set_var("PORTAL_SESSION_ARCHIVE_BACKEND", "gopher");
        assert!(archive_config_from_env().is_err());

        std::env::set_var("PORTAL_SESSION_ARCHIVE_BACKEND", "local");
        std::env::remove_var("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT");
        assert!(archive_config_from_env().is_err());

        std::env::set_var("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT", "/tmp/x");
        let cfg = archive_config_from_env().unwrap().unwrap();
        assert!(cfg.transcripts && cfg.media);

        std::env::remove_var("PORTAL_SESSION_ARCHIVE_BACKEND");
        std::env::remove_var("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT");
    }
}

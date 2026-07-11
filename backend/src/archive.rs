//! Long-term session archive storage (#1258).
//!
//! Completed sessions are archived outside the hot Postgres tables so
//! message/session retention can stay bounded without losing audit or
//! usage history. Backends: local filesystem and S3-compatible object
//! storage (via `object_store`). Reading/analytics happens in a separate
//! viewer tool — this module only writes (and re-reads for the
//! merge-on-rearchive invariant).
//!
//! ## Object layout (schema v1)
//!
//! ```text
//! v1/users/{user_id}/sessions/{session_id}/manifest.json
//! v1/users/{user_id}/sessions/{session_id}/messages.ndjson.zst
//! ```
//!
//! Transcripts are always zstd-compressed — they are text-heavy NDJSON
//! and shrink dramatically; there is deliberately no plaintext option.
//! Manifests stay plain JSON so external tools can list/scan them cheaply.
//!
//! Keys are deterministic (stable ids only, no user-controlled segments),
//! so re-archiving a session overwrites in place — the sweep re-archives
//! whenever `sessions.last_activity` advances past `sessions.archived_at`,
//! making the whole pipeline idempotent.
//!
//! ## Trust model
//!
//! The archive backend is operator-controlled infrastructure: anyone with
//! access to the root path (or, later, the bucket) already holds
//! deployment-level access. Raw message bodies are archived deliberately —
//! the feature exists to make provider-bound content auditable — but
//! transport/auth material (tokens, cookies, secrets) is never part of
//! session content and must never be added to manifests.

use chrono::NaiveDateTime;
use object_store::ObjectStoreExt as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Bump when the manifest shape or object layout changes incompatibly.
pub const ARCHIVE_SCHEMA_VERSION: u32 = 1;

/// How often the archival sweep runs.
pub const ARCHIVE_SWEEP_INTERVAL_SECS: u64 = 300;

/// A session must have been idle this long before it's archived, so a
/// briefly-disconnected session isn't archived mid-flap (reconnects are
/// routine; see the #1256 lifecycle work).
pub const ARCHIVE_IDLE_SECS: i64 = 3600;

/// Sessions archived per sweep tick, so one sweep can't monopolize the DB.
pub const ARCHIVE_SWEEP_BATCH: i64 = 25;

/// zstd level for transcript bodies. Archival is write-once/read-rare and
/// runs on the blocking pool, so a higher-than-default level is a good
/// trade: noticeably smaller objects for a little more CPU.
const ZSTD_LEVEL: i32 = 9;

/// The manifest's `transcript.compression` value. Fixed — transcripts are
/// always zstd; the field exists so external viewers stay self-describing
/// if a future schema version ever changes the codec.
pub const TRANSCRIPT_COMPRESSION: &str = "zstd";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Which object store the archive writes to.
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
    Ok(Some(ArchiveConfig {
        backend,
        transcripts,
    }))
}

// ---------------------------------------------------------------------------
// Manifest / bundle types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ArchiveTokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
    pub thinking: i64,
    pub subagent: i64,
}

/// Turn-level aggregates for the manifest, sourced from `turn_metrics`.
///
/// Known v1 gaps, documented deliberately: **rate-limit event counts and
/// reconnect counts have no durable DB source today** (limit events are
/// transient wire frames; reconnects live only in logs), so they cannot
/// appear in manifests until something persists them. `errored` counts
/// turns with `is_error`, which subsumes limit-terminated turns.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ArchiveTurnStats {
    pub count: i64,
    pub errored: i64,
    /// Stop-reason histogram (BTreeMap for deterministic serialization).
    pub stop_reasons: BTreeMap<String, i64>,
    /// Distinct models observed, sorted.
    pub models: Vec<String>,
    /// Service-tier histogram (e.g. "standard" / "priority").
    pub service_tiers: BTreeMap<String, i64>,
    /// Total tool invocations across all turns.
    pub tool_calls: i64,
    /// Total stream restarts (auto-retried turns) across all turns.
    pub stream_restarts: i64,
    /// Sum of per-turn wall-clock durations, milliseconds.
    pub total_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArchiveTranscriptInfo {
    pub object_key: String,
    pub compression: String,
    pub message_count: i64,
    pub bytes: u64,
}

/// The per-session archive manifest (schema v1). Analytics and admin
/// surfaces read this; they must never need the transcript body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionArchiveManifest {
    pub schema_version: u32,
    pub session_id: Uuid,
    pub user_id: Uuid,
    /// Raw email deliberately included for admin reporting (see module
    /// docs on the trust model).
    pub owner_email: String,
    pub owner_name: Option<String>,
    pub session_name: String,
    pub agent_type: String,
    pub status: String,
    pub working_directory: String,
    pub hostname: String,
    pub git_branch: Option<String>,
    pub repo_url: Option<String>,
    pub pr_url: Option<String>,
    pub client_version: Option<String>,
    pub created_at: NaiveDateTime,
    pub last_activity: NaiveDateTime,
    pub archived_at: NaiveDateTime,
    /// Message-count histogram by role (user/assistant/...).
    pub message_counts: BTreeMap<String, i64>,
    pub tokens: ArchiveTokenTotals,
    pub total_cost_usd: f64,
    pub turns: ArchiveTurnStats,
    /// Present iff transcript archival is enabled and messages existed.
    pub transcript: Option<ArchiveTranscriptInfo>,
}

/// One transcript line, serialized as NDJSON. `content` is the raw stored
/// message JSON, embedded as a JSON value so the archive round-trips the
/// wire content byte-for-byte semantically.
///
/// `id` is the message row's UUID — the merge key that lets a re-archive
/// UNION the existing archived transcript with whatever remains in the hot
/// DB (phase 2 trims hot messages after archival; a later re-archive must
/// never shrink the archived transcript).
#[derive(Debug, Serialize, Deserialize)]
pub struct ArchiveMessageLine {
    pub id: Uuid,
    pub role: String,
    pub created_at: NaiveDateTime,
    pub agent_type: String,
    pub content: serde_json::Value,
}

/// Everything needed to write one session's archive.
pub struct SessionArchiveBundle {
    pub manifest: SessionArchiveManifest,
    /// NDJSON transcript body (uncompressed); `None` in metadata-only mode.
    pub transcript_ndjson: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Object keys
// ---------------------------------------------------------------------------

pub fn manifest_key(user_id: Uuid, session_id: Uuid) -> String {
    format!("v1/users/{user_id}/sessions/{session_id}/manifest.json")
}

pub fn transcript_key(user_id: Uuid, session_id: Uuid) -> String {
    format!("v1/users/{user_id}/sessions/{session_id}/messages.ndjson.zst")
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Store plus the settings the archival sweep needs at write time.
pub struct ArchiveRuntime {
    pub store: ArchiveStore,
    pub config: ArchiveConfig,
    pub stats: ArchiveStats,
}

impl ArchiveRuntime {
    /// Build the runtime, constructing the backing store. Fails fast on
    /// invalid S3 configuration (bad bucket, missing region, no runtime).
    pub fn new(config: ArchiveConfig) -> Result<Self, String> {
        Ok(Self {
            store: ArchiveStore::from_config(&config)?,
            config,
            stats: ArchiveStats::default(),
        })
    }
}

/// Process-lifetime archival counters (#1258 phase 2 observability).
/// Logged by the sweep; a later phase surfaces them on an admin endpoint.
#[derive(Default)]
pub struct ArchiveStats {
    pub archived_total: std::sync::atomic::AtomicU64,
    pub failed_total: std::sync::atomic::AtomicU64,
    pub bytes_written: std::sync::atomic::AtomicU64,
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl ArchiveStats {
    pub fn record_success(&self, bytes: u64) {
        use std::sync::atomic::Ordering;
        self.archived_total.fetch_add(1, Ordering::Relaxed);
        self.bytes_written.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_failure(&self, error: &str) {
        use std::sync::atomic::Ordering;
        self.failed_total.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last) = self.last_error.lock() {
            *last = Some(error.to_string());
        }
    }
}

/// Union an existing archived transcript with the messages currently in the
/// hot DB, keyed by message id (#1258 phase 2). Retention may have trimmed
/// hot rows that only exist in the archive, and the DB may hold rows newer
/// than the archive — the merge keeps both, ordered by `created_at` (id as
/// a deterministic tiebreaker). A re-archive can therefore never shrink an
/// archived transcript.
pub fn merge_transcript_lines(
    existing: Vec<ArchiveMessageLine>,
    current: Vec<ArchiveMessageLine>,
) -> Vec<ArchiveMessageLine> {
    let mut by_id: BTreeMap<Uuid, ArchiveMessageLine> = BTreeMap::new();
    for line in existing {
        by_id.insert(line.id, line);
    }
    // Current DB rows win on id collision (content is immutable in
    // practice; this just prefers the freshest serialization).
    for line in current {
        by_id.insert(line.id, line);
    }
    let mut merged: Vec<ArchiveMessageLine> = by_id.into_values().collect();
    merged.sort_by_key(|line| (line.created_at, line.id));
    merged
}

/// Typed archive store. An enum (not a trait object) so backends stay a
/// closed, compiler-checked set.
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
    /// object exists yet. Used by the merge-on-rearchive path.
    pub fn read_transcript_lines(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> std::io::Result<Option<Vec<ArchiveMessageLine>>> {
        let raw = match self {
            Self::Local(store) => {
                match std::fs::read(store.object_path(&transcript_key(user_id, session_id))) {
                    Ok(bytes) => bytes,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e),
                }
            }
            Self::Object(store) => match store.get_bytes(&transcript_key(user_id, session_id)) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return Ok(None),
                Err(e) => return Err(e),
            },
        };
        parse_transcript_ndjson(&zstd::decode_all(raw.as_slice())?).map(Some)
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

    fn put_session_archive(&self, bundle: &SessionArchiveBundle) -> std::io::Result<()> {
        let m = &bundle.manifest;
        if let Some(ndjson) = &bundle.transcript_ndjson {
            let body = zstd::encode_all(ndjson.as_slice(), ZSTD_LEVEL)?;
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
    root: PathBuf,
}

impl LocalArchiveStore {
    fn object_path(&self, key: &str) -> PathBuf {
        // Keys are built exclusively by `manifest_key`/`transcript_key`
        // from UUIDs and fixed literals — no user-controlled segments.
        self.root.join(key)
    }

    fn write_atomic(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        let path = self.object_path(key);
        let dir = path.parent().expect("object keys always have a parent");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{}.tmp",
            path.file_name()
                .expect("object keys always have a file name")
                .to_string_lossy()
        ));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    }

    fn put_session_archive(&self, bundle: &SessionArchiveBundle) -> std::io::Result<()> {
        let m = &bundle.manifest;
        if let Some(ndjson) = &bundle.transcript_ndjson {
            let body = zstd::encode_all(ndjson.as_slice(), ZSTD_LEVEL)?;
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
    parse_transcript_ndjson(&zstd::decode_all(raw.as_slice())?)
}

fn parse_transcript_ndjson(ndjson: &[u8]) -> std::io::Result<Vec<ArchiveMessageLine>> {
    String::from_utf8_lossy(ndjson)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad line: {e}"))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(user_id: Uuid, session_id: Uuid) -> SessionArchiveManifest {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        SessionArchiveManifest {
            schema_version: ARCHIVE_SCHEMA_VERSION,
            session_id,
            user_id,
            owner_email: "t@t".into(),
            owner_name: None,
            session_name: "s".into(),
            agent_type: "claude".into(),
            status: "disconnected".into(),
            working_directory: "/w".into(),
            hostname: "h".into(),
            git_branch: None,
            repo_url: None,
            pr_url: None,
            client_version: None,
            created_at: t,
            last_activity: t,
            archived_at: t,
            message_counts: BTreeMap::from([("user".into(), 2)]),
            tokens: ArchiveTokenTotals::default(),
            total_cost_usd: 0.5,
            turns: ArchiveTurnStats::default(),
            transcript: None,
        }
    }

    #[test]
    fn keys_are_deterministic_and_partitioned() {
        let u = Uuid::from_u128(1);
        let s = Uuid::from_u128(2);
        assert_eq!(
            manifest_key(u, s),
            format!("v1/users/{u}/sessions/{s}/manifest.json")
        );
        assert_eq!(
            transcript_key(u, s),
            format!("v1/users/{u}/sessions/{s}/messages.ndjson.zst")
        );
    }

    fn bundle_with_one_line(u: Uuid, s: Uuid) -> (SessionArchiveManifest, SessionArchiveBundle) {
        let line = ArchiveMessageLine {
            id: Uuid::from_u128(11),
            role: "user".into(),
            created_at: chrono::NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            agent_type: "claude".into(),
            content: serde_json::json!({"type": "user", "text": "hello"}),
        };
        let ndjson = format!("{}\n", serde_json::to_string(&line).unwrap()).into_bytes();

        let mut m = manifest(u, s);
        m.transcript = Some(ArchiveTranscriptInfo {
            object_key: transcript_key(u, s),
            compression: TRANSCRIPT_COMPRESSION.into(),
            message_count: 1,
            bytes: ndjson.len() as u64,
        });
        let bundle = SessionArchiveBundle {
            manifest: m.clone(),
            transcript_ndjson: Some(ndjson),
        };
        (m, bundle)
    }

    #[test]
    fn local_roundtrip_and_idempotent_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ArchiveStore::Local(LocalArchiveStore {
            root: tmp.path().to_path_buf(),
        });
        let (u, s) = (Uuid::from_u128(7), Uuid::from_u128(9));
        let (m, bundle) = bundle_with_one_line(u, s);

        store.put_session_archive(&bundle).unwrap();
        // Overwrite with the same deterministic keys must succeed.
        store.put_session_archive(&bundle).unwrap();

        let got = store.get_session_manifest(u, s).unwrap().expect("manifest");
        assert_eq!(got, m);

        let lines = read_transcript(tmp.path(), u, s).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].content["text"], "hello");

        // The on-disk transcript must actually be zstd, not plaintext.
        let raw = std::fs::read(tmp.path().join(transcript_key(u, s))).unwrap();
        assert_eq!(&raw[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic bytes");

        // No temp files left behind (write_atomic names them `.{name}.tmp`).
        let leftovers: Vec<_> = walk(tmp.path())
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".tmp"))
            })
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
    }

    /// The `Object` (S3) backend, exercised via `object_store`'s in-memory
    /// implementation from a plain thread — same block-on-handle path the
    /// blocking sweep uses in production.
    #[test]
    fn object_store_roundtrip_and_missing_reads() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let store = ArchiveStore::Object(ObjectArchiveStore {
            store: std::sync::Arc::new(object_store::memory::InMemory::new()),
            prefix: Some("portal-archive".into()),
            handle: rt.handle().clone(),
        });
        let (u, s) = (Uuid::from_u128(7), Uuid::from_u128(9));

        assert!(store.get_session_manifest(u, s).unwrap().is_none());
        assert!(store.read_transcript_lines(u, s).unwrap().is_none());

        let (m, bundle) = bundle_with_one_line(u, s);
        store.put_session_archive(&bundle).unwrap();
        // Deterministic-key overwrite must succeed (re-archive path).
        store.put_session_archive(&bundle).unwrap();

        let got = store.get_session_manifest(u, s).unwrap().expect("manifest");
        assert_eq!(got, m);

        let lines = store
            .read_transcript_lines(u, s)
            .unwrap()
            .expect("transcript");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].content["text"], "hello");
    }

    /// #1258 phase 2: the merge is a union by message id, ordered by
    /// (created_at, id), with current DB rows winning id collisions — a
    /// re-archive can only grow a transcript, never shrink it.
    #[test]
    fn merge_never_shrinks_and_orders_deterministically() {
        let t0 = chrono::NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let line = |id: u128, secs: u32, text: &str| ArchiveMessageLine {
            id: Uuid::from_u128(id),
            role: "user".into(),
            created_at: t0 + chrono::Duration::seconds(secs as i64),
            agent_type: "claude".into(),
            content: serde_json::json!({ "text": text }),
        };

        // Archive holds 1,2 (retention later trimmed them from the DB);
        // DB holds 2 (fresher serialization) and 3 (new).
        let existing = vec![line(1, 0, "one"), line(2, 10, "two-old")];
        let current = vec![line(2, 10, "two-new"), line(3, 20, "three")];

        let merged = merge_transcript_lines(existing, current);
        let texts: Vec<&str> = merged
            .iter()
            .map(|l| l.content["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, vec!["one", "two-new", "three"]);

        // Idempotent: merging the result with itself changes nothing.
        let again = merge_transcript_lines(
            merged.iter().map(clone_line).collect(),
            merged.iter().map(clone_line).collect(),
        );
        assert_eq!(again.len(), merged.len());
    }

    fn clone_line(l: &ArchiveMessageLine) -> ArchiveMessageLine {
        ArchiveMessageLine {
            id: l.id,
            role: l.role.clone(),
            created_at: l.created_at,
            agent_type: l.agent_type.clone(),
            content: l.content.clone(),
        }
    }

    #[test]
    fn missing_manifest_reads_as_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ArchiveStore::Local(LocalArchiveStore {
            root: tmp.path().to_path_buf(),
        });
        assert!(store
            .get_session_manifest(Uuid::from_u128(1), Uuid::from_u128(2))
            .unwrap()
            .is_none());
    }

    #[test]
    fn config_parses_and_fails_fast() {
        // NOTE: env-var tests mutate process state; keep them in ONE test
        // so parallel test threads can't race each other.
        let clear = || {
            for k in [
                "PORTAL_SESSION_ARCHIVE_BACKEND",
                "PORTAL_SESSION_ARCHIVE_LOCAL_ROOT",
                "PORTAL_SESSION_ARCHIVE_COMPRESS",
                "PORTAL_SESSION_ARCHIVE_TRANSCRIPTS",
                "PORTAL_SESSION_ARCHIVE_S3_BUCKET",
                "PORTAL_SESSION_ARCHIVE_S3_PREFIX",
            ] {
                std::env::remove_var(k);
            }
        };

        clear();
        assert!(archive_config_from_env().unwrap().is_none(), "default off");

        std::env::set_var("PORTAL_SESSION_ARCHIVE_BACKEND", "local");
        assert!(
            archive_config_from_env().is_err(),
            "local without root must fail fast"
        );

        std::env::set_var("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT", "/tmp/a");
        let cfg = archive_config_from_env().unwrap().expect("enabled");
        assert!(
            matches!(cfg.backend, ArchiveBackendConfig::Local { .. }),
            "local backend"
        );
        assert!(cfg.transcripts, "transcripts default on when enabled");

        // The removed compression knob must be rejected, not ignored.
        std::env::set_var("PORTAL_SESSION_ARCHIVE_COMPRESS", "none");
        assert!(
            archive_config_from_env().is_err(),
            "removed COMPRESS knob fails fast"
        );
        std::env::remove_var("PORTAL_SESSION_ARCHIVE_COMPRESS");

        std::env::set_var("PORTAL_SESSION_ARCHIVE_BACKEND", "s3");
        assert!(
            archive_config_from_env().is_err(),
            "s3 without bucket must fail fast"
        );

        std::env::set_var("PORTAL_SESSION_ARCHIVE_S3_BUCKET", "my-bucket");
        std::env::set_var("PORTAL_SESSION_ARCHIVE_S3_PREFIX", "/portal/archive/");
        let cfg = archive_config_from_env().unwrap().expect("enabled");
        match cfg.backend {
            ArchiveBackendConfig::S3 { bucket, prefix } => {
                assert_eq!(bucket, "my-bucket");
                assert_eq!(prefix.as_deref(), Some("portal/archive"), "slashes trimmed");
            }
            other => panic!("expected S3 backend, got {other:?}"),
        }

        std::env::set_var("PORTAL_SESSION_ARCHIVE_BACKEND", "bogus");
        assert!(archive_config_from_env().is_err());
        clear();
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }
}

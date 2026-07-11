//! Long-term session archive storage (#1258, phase 1).
//!
//! Completed sessions are archived outside the hot Postgres tables so
//! message/session retention can stay bounded without losing audit or
//! usage history. Phase 1 ships the typed store (local filesystem
//! backend), the versioned object layout, manifest construction, and the
//! periodic archival sweep; later phases add retention integration, an
//! S3-compatible backend, rollups, and UI.
//!
//! ## Object layout (schema v1)
//!
//! ```text
//! v1/users/{user_id}/sessions/{session_id}/manifest.json
//! v1/users/{user_id}/sessions/{session_id}/messages.ndjson.zst
//! ```
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

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveCompression {
    Zstd,
    None,
}

impl ArchiveCompression {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::None => "none",
        }
    }

    fn transcript_suffix(&self) -> &'static str {
        match self {
            Self::Zstd => "messages.ndjson.zst",
            Self::None => "messages.ndjson",
        }
    }
}

/// Validated archive configuration. `None` anywhere upstream means the
/// feature is disabled (the default, including on hosted deployments).
#[derive(Debug, Clone)]
pub struct ArchiveConfig {
    pub local_root: PathBuf,
    pub compression: ArchiveCompression,
    /// When false, only manifests are archived (metadata/rollup mode);
    /// transcripts stay subject to normal DB retention.
    pub transcripts: bool,
}

/// Parse archive settings from the environment. Fail-fast: partial or
/// contradictory config is an error at startup, not a silent fallback.
pub fn archive_config_from_env() -> Result<Option<ArchiveConfig>, String> {
    let backend =
        std::env::var("PORTAL_SESSION_ARCHIVE_BACKEND").unwrap_or_else(|_| "disabled".to_string());
    match backend.as_str() {
        "disabled" => Ok(None),
        "local" => {
            let root = std::env::var("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT").map_err(|_| {
                "PORTAL_SESSION_ARCHIVE_BACKEND=local requires \
                 PORTAL_SESSION_ARCHIVE_LOCAL_ROOT to be set"
                    .to_string()
            })?;
            if root.trim().is_empty() {
                return Err("PORTAL_SESSION_ARCHIVE_LOCAL_ROOT must not be empty".to_string());
            }
            let compression = match std::env::var("PORTAL_SESSION_ARCHIVE_COMPRESS")
                .unwrap_or_else(|_| "zstd".to_string())
                .as_str()
            {
                "zstd" => ArchiveCompression::Zstd,
                "none" => ArchiveCompression::None,
                other => {
                    return Err(format!(
                        "PORTAL_SESSION_ARCHIVE_COMPRESS must be `zstd` or `none`, got `{other}`"
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
                local_root: PathBuf::from(root),
                compression,
                transcripts,
            }))
        }
        "s3" => Err(
            "PORTAL_SESSION_ARCHIVE_BACKEND=s3 is not implemented yet (#1258 phase 3); \
             use `local` or `disabled`"
                .to_string(),
        ),
        other => Err(format!(
            "PORTAL_SESSION_ARCHIVE_BACKEND must be `disabled`, `local`, or `s3`, got `{other}`"
        )),
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ArchiveTurnStats {
    pub count: i64,
    pub errored: i64,
    /// Stop-reason histogram (BTreeMap for deterministic serialization).
    pub stop_reasons: BTreeMap<String, i64>,
    /// Distinct models observed, sorted.
    pub models: Vec<String>,
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

pub fn transcript_key(user_id: Uuid, session_id: Uuid, compression: ArchiveCompression) -> String {
    format!(
        "v1/users/{user_id}/sessions/{session_id}/{}",
        compression.transcript_suffix()
    )
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Store plus the settings the archival sweep needs at write time.
pub struct ArchiveRuntime {
    pub store: ArchiveStore,
    pub config: ArchiveConfig,
}

impl ArchiveRuntime {
    pub fn new(config: ArchiveConfig) -> Self {
        Self {
            store: ArchiveStore::from_config(&config),
            config,
        }
    }
}

/// Typed archive store. An enum (not a trait object) so backends stay a
/// closed, compiler-checked set; phase 3 adds an `S3` variant.
pub enum ArchiveStore {
    Local(LocalArchiveStore),
}

impl ArchiveStore {
    pub fn from_config(config: &ArchiveConfig) -> Self {
        Self::Local(LocalArchiveStore {
            root: config.local_root.clone(),
        })
    }

    /// Write a session's archive: transcript first, manifest last — a
    /// manifest's existence implies its transcript object is complete.
    pub fn put_session_archive(
        &self,
        bundle: &SessionArchiveBundle,
        compression: ArchiveCompression,
    ) -> std::io::Result<()> {
        match self {
            Self::Local(store) => store.put_session_archive(bundle, compression),
        }
    }

    pub fn get_session_manifest(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> std::io::Result<Option<SessionArchiveManifest>> {
        match self {
            Self::Local(store) => store.get_session_manifest(user_id, session_id),
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

    fn put_session_archive(
        &self,
        bundle: &SessionArchiveBundle,
        compression: ArchiveCompression,
    ) -> std::io::Result<()> {
        let m = &bundle.manifest;
        if let Some(ndjson) = &bundle.transcript_ndjson {
            let body: Vec<u8> = match compression {
                ArchiveCompression::Zstd => zstd::encode_all(ndjson.as_slice(), 0)?,
                ArchiveCompression::None => ndjson.clone(),
            };
            self.write_atomic(&transcript_key(m.user_id, m.session_id, compression), &body)?;
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
    compression: ArchiveCompression,
) -> std::io::Result<Vec<ArchiveMessageLine>> {
    let path = root.join(transcript_key(user_id, session_id, compression));
    let raw = std::fs::read(path)?;
    let ndjson = match compression {
        ArchiveCompression::Zstd => zstd::decode_all(raw.as_slice())?,
        ArchiveCompression::None => raw,
    };
    String::from_utf8_lossy(&ndjson)
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
            transcript_key(u, s, ArchiveCompression::Zstd),
            format!("v1/users/{u}/sessions/{s}/messages.ndjson.zst")
        );
        assert_eq!(
            transcript_key(u, s, ArchiveCompression::None),
            format!("v1/users/{u}/sessions/{s}/messages.ndjson")
        );
    }

    #[test]
    fn local_roundtrip_and_idempotent_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ArchiveStore::Local(LocalArchiveStore {
            root: tmp.path().to_path_buf(),
        });
        let (u, s) = (Uuid::from_u128(7), Uuid::from_u128(9));

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
            object_key: transcript_key(u, s, ArchiveCompression::Zstd),
            compression: "zstd".into(),
            message_count: 1,
            bytes: ndjson.len() as u64,
        });
        let bundle = SessionArchiveBundle {
            manifest: m.clone(),
            transcript_ndjson: Some(ndjson),
        };

        store
            .put_session_archive(&bundle, ArchiveCompression::Zstd)
            .unwrap();
        // Overwrite with the same deterministic keys must succeed.
        store
            .put_session_archive(&bundle, ArchiveCompression::Zstd)
            .unwrap();

        let got = store.get_session_manifest(u, s).unwrap().expect("manifest");
        assert_eq!(got, m);

        let lines = read_transcript(tmp.path(), u, s, ArchiveCompression::Zstd).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].content["text"], "hello");

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
        assert_eq!(cfg.compression, ArchiveCompression::Zstd);
        assert!(cfg.transcripts, "transcripts default on when enabled");

        std::env::set_var("PORTAL_SESSION_ARCHIVE_BACKEND", "s3");
        assert!(archive_config_from_env().is_err(), "s3 is phase 3");

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

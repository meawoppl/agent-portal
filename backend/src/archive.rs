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

// ---------------------------------------------------------------------------
// Format + store layer — moved to the `archive-format` crate (#1288) so the
// standalone archive viewer reads archives without linking the backend.
// Re-exported here so every existing `crate::archive::X` path keeps working.
// ---------------------------------------------------------------------------

pub use archive_format::{
    archive_config_from_env, manifest_key, media_key, media_meta_key, merge_transcript_lines,
    read_transcript, transcript_key, zstd_decode, zstd_encode, ArchiveBackendConfig, ArchiveConfig,
    ArchiveMessageLine, ArchiveStore, ArchiveTokenTotals, ArchiveTranscriptInfo, ArchiveTurnStats,
    ArchivedMediaMeta, LocalArchiveStore, MediaEntry, ObjectArchiveStore, SessionArchiveBundle,
    SessionArchiveManifest, ARCHIVE_SCHEMA_VERSION, TRANSCRIPT_COMPRESSION,
};

/// How often the archival sweep runs.
pub const ARCHIVE_SWEEP_INTERVAL_SECS: u64 = 300;

/// A session must have been idle this long before it's archived, so a
/// briefly-disconnected session isn't archived mid-flap (reconnects are
/// routine; see the #1256 lifecycle work).
pub const ARCHIVE_IDLE_SECS: i64 = 3600;

/// Sessions archived per sweep tick, so one sweep can't monopolize the DB.
pub const ARCHIVE_SWEEP_BATCH: i64 = 25;

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
    /// malformed S3 configuration (invalid bucket name, missing region,
    /// no runtime) — but bucket reachability and credential validity are
    /// only proven by the first write; watch `SESSION_ARCHIVE_FAILED`
    /// after enabling.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

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
            status: shared::SessionStatus::Disconnected.as_str().into(),
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
            media: None,
            launcher_id: None,
            launcher_version: None,
            scheduled_task_id: None,
            claude_args: Vec::new(),
            archived_by_version: None,
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
        let store = ArchiveStore::Local(LocalArchiveStore::new(tmp.path().to_path_buf()));
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
        let store = ArchiveStore::Object(ObjectArchiveStore::new(
            std::sync::Arc::new(object_store::memory::InMemory::new()),
            Some("portal-archive".into()),
            rt.handle().clone(),
        ));
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
        let store = ArchiveStore::Local(LocalArchiveStore::new(tmp.path().to_path_buf()));
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
                "PORTAL_SESSION_ARCHIVE_MEDIA",
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
        assert!(cfg.media, "media write-through defaults on when enabled");

        // The media knob must be a strict bool, not silently coerced.
        std::env::set_var("PORTAL_SESSION_ARCHIVE_MEDIA", "false");
        assert!(
            !archive_config_from_env().unwrap().unwrap().media,
            "media=false disables write-through"
        );
        std::env::set_var("PORTAL_SESSION_ARCHIVE_MEDIA", "bogus");
        assert!(
            archive_config_from_env().is_err(),
            "non-bool media knob fails fast"
        );
        std::env::remove_var("PORTAL_SESSION_ARCHIVE_MEDIA");

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

    fn media_meta(media_id: Uuid) -> ArchivedMediaMeta {
        ArchivedMediaMeta {
            media_id,
            kind: "image".into(),
            content_type: "image/png".into(),
            filename: Some("plot.png".into()),
            bytes: 5,
            uploaded_at: chrono::NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        }
    }

    #[test]
    fn media_roundtrip_local_and_object() {
        // Local backend.
        let tmp = tempfile::tempdir().unwrap();
        let local = ArchiveStore::Local(LocalArchiveStore::new(tmp.path().to_path_buf()));
        // Object (S3) backend via the in-memory store.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let object = ArchiveStore::Object(ObjectArchiveStore::new(
            std::sync::Arc::new(object_store::memory::InMemory::new()),
            Some("portal-archive".into()),
            rt.handle().clone(),
        ));

        for store in [&local, &object] {
            let (u, s, m) = (Uuid::from_u128(7), Uuid::from_u128(9), Uuid::from_u128(42));
            // Missing reads before any write.
            assert!(store.get_media_meta(u, s, m).unwrap().is_none());
            assert!(store.get_media_bytes(u, s, m).unwrap().is_none());

            let meta = media_meta(m);
            store.put_media(u, s, &meta, b"hello").unwrap();
            // Idempotent overwrite with deterministic key.
            store.put_media(u, s, &meta, b"hello").unwrap();

            assert_eq!(store.get_media_bytes(u, s, m).unwrap().unwrap(), b"hello");
            assert_eq!(store.get_media_meta(u, s, m).unwrap().unwrap(), meta);
        }
    }

    #[test]
    fn manifest_media_section_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ArchiveStore::Local(LocalArchiveStore::new(tmp.path().to_path_buf()));
        let (u, s) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let m = Uuid::from_u128(3);
        let mut manifest = manifest(u, s);
        manifest.media = Some(vec![MediaEntry {
            media_id: m,
            kind: "video".into(),
            content_type: "video/mp4".into(),
            bytes: 123,
            object_key: media_key(u, s, m),
            uploaded_at: manifest.created_at,
        }]);
        let bundle = SessionArchiveBundle {
            manifest: manifest.clone(),
            transcript_ndjson: None,
        };
        store.put_session_archive(&bundle).unwrap();
        let got = store.get_session_manifest(u, s).unwrap().expect("manifest");
        assert_eq!(got, manifest);
        assert_eq!(got.media.unwrap().len(), 1);
    }

    #[test]
    fn old_manifest_without_media_field_parses() {
        // A manifest serialized before the `media` field existed must still
        // deserialize (additive optional field → defaults to None), and a
        // media-less manifest must not emit the key (byte-compat).
        let u = Uuid::from_u128(1);
        let s = Uuid::from_u128(2);
        let m = manifest(u, s);
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            !json.contains("\"media\""),
            "media-less manifest must omit the key: {json}"
        );

        // Hand-build a manifest JSON object with the `media` key entirely
        // absent (simulating an old writer) and confirm it round-trips.
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.as_object_mut().unwrap().remove("media").is_none());
        let parsed: SessionArchiveManifest = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.media, None);
    }

    #[test]
    fn manifest_provenance_fields_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ArchiveStore::Local(LocalArchiveStore::new(tmp.path().to_path_buf()));
        let (u, s) = (Uuid::from_u128(4), Uuid::from_u128(5));
        let mut manifest = manifest(u, s);
        manifest.launcher_id = Some(Uuid::from_u128(6));
        manifest.launcher_version = Some("2.13.42".into());
        manifest.scheduled_task_id = Some(Uuid::from_u128(7));
        manifest.claude_args = vec!["--model".into(), "opus".into()];
        manifest.archived_by_version = Some("2.13.99".into());
        let bundle = SessionArchiveBundle {
            manifest: manifest.clone(),
            transcript_ndjson: None,
        };
        store.put_session_archive(&bundle).unwrap();
        let got = store.get_session_manifest(u, s).unwrap().expect("manifest");
        assert_eq!(got, manifest);
    }

    #[test]
    fn old_manifest_without_provenance_fields_parses() {
        // A manifest written before the provenance fields existed must still
        // deserialize (each is additive/optional → empty default), and a
        // manifest with none of them set must omit every key (byte-compat).
        let u = Uuid::from_u128(1);
        let s = Uuid::from_u128(2);
        let m = manifest(u, s);
        let json = serde_json::to_string(&m).unwrap();
        for key in [
            "launcher_id",
            "launcher_version",
            "scheduled_task_id",
            "claude_args",
            "archived_by_version",
        ] {
            assert!(
                !json.contains(&format!("\"{key}\"")),
                "provenance-less manifest must omit `{key}`: {json}"
            );
        }

        // Simulate an old writer: strip the keys entirely and confirm the
        // manifest still round-trips to empty defaults.
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = value.as_object_mut().unwrap();
        for key in [
            "launcher_id",
            "launcher_version",
            "scheduled_task_id",
            "claude_args",
            "archived_by_version",
        ] {
            assert!(obj.remove(key).is_none());
        }
        let parsed: SessionArchiveManifest = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.launcher_id, None);
        assert_eq!(parsed.launcher_version, None);
        assert_eq!(parsed.scheduled_task_id, None);
        assert!(parsed.claude_args.is_empty());
        assert_eq!(parsed.archived_by_version, None);
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

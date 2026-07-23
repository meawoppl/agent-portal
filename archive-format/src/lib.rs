//! Long-term session archive format (#1258).
//!
//! This crate is the *format* layer for the session archive: the serde types
//! (manifests, transcript lines, media sidecars), the deterministic object-key
//! derivation, the zstd transcript codec, and the read-side store
//! (local filesystem + S3-compatible `object_store`). It is shared by two
//! consumers so neither has to link the other:
//!
//! * the **backend** writes archives from the archival sweep, and
//! * a standalone **archive/history viewer** reads them back.
//!
//! Backend-only glue (the sweep, DB-facing bundle assembly, `ArchiveRuntime`
//! wiring, stats, and marker emissions) stays in the backend crate.
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

pub mod store;
pub use store::*;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Bump when the manifest shape or object layout changes incompatibly.
pub const ARCHIVE_SCHEMA_VERSION: u32 = 1;

/// zstd level for transcript bodies. Archival is write-once/read-rare and
/// runs on the blocking pool, so a higher-than-default level is a good
/// trade: noticeably smaller objects for a little more CPU.
const ZSTD_LEVEL: i32 = 9;

/// The manifest's `transcript.compression` value. Fixed — transcripts are
/// always zstd; the field exists so external viewers stay self-describing
/// if a future schema version ever changes the codec.
pub const TRANSCRIPT_COMPRESSION: &str = "zstd";

/// zstd-compress a transcript body at the archive's fixed [`ZSTD_LEVEL`].
/// Centralized here so writer (backend) and reader (viewer) share one codec.
pub fn zstd_encode(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    zstd::encode_all(bytes, ZSTD_LEVEL)
}

/// Decompress a zstd transcript body.
pub fn zstd_decode(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    zstd::decode_all(bytes)
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
    /// Media blobs (`agent-portal show`) whose bytes were written through to
    /// the archive for this session, one entry per surviving blob.
    ///
    /// **Schema-compat (why this stays v1):** the field is additive and
    /// optional — `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// means an old manifest written before this field existed deserializes
    /// with `media == None`, and a manifest with no media re-serializes
    /// byte-identically to the old shape (the key is omitted). Neither the
    /// object layout nor any existing field changes, so external viewers that
    /// don't know the field are unaffected. That is exactly the additive-change
    /// contract [`ARCHIVE_SCHEMA_VERSION`] guards, so it is **not** bumped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Vec<MediaEntry>>,

    // --- Provenance (all additive within schema v1) ---
    //
    // These follow the same additive-compat contract as `media` above:
    // each is `#[serde(default, skip_serializing_if = …)]`, so a manifest
    // written before the field existed deserializes to the empty value and
    // a manifest without the datum re-serializes byte-identically (key
    // omitted). No existing field or the object layout changes, so
    // [`ARCHIVE_SCHEMA_VERSION`] is **not** bumped.
    /// The launcher that spawned this session (`sessions.launcher_id`), or
    /// `None` for proxy-direct sessions. Pairs with `launcher_version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher_id: Option<Uuid>,
    /// The launcher's self-reported version at launch time
    /// (`sessions.launcher_version`), captured when the session row was
    /// created. **Last-known-at-launch**: a mid-session launcher
    /// auto-update does not refresh it. `None` for non-launcher sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher_version: Option<String>,
    /// The scheduled (cron) task that spawned this session
    /// (`sessions.scheduled_task_id`), linking an archived session back to
    /// the automation that created it. `None` for interactively-launched
    /// sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<Uuid>,
    /// Extra CLI arguments the agent binary was launched with
    /// (`sessions.claude_args`), e.g. `["--model", "opus"]`. Reveals model
    /// pinning and other launch-time behavior knobs. These are agent CLI
    /// flags only — portal auth travels via a separate proxy token, never
    /// here — so they carry no transport/auth secrets. Empty (and thus
    /// omitted) for sessions launched with no extra args.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claude_args: Vec<String>,
    /// `shared::VERSION` of the backend that performed this archive write.
    /// **Per-write**: a re-archive by a newer backend stamps the newer
    /// version, so this records who last wrote the object, not who first
    /// created the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by_version: Option<String>,
}

/// One archived media blob referenced from a manifest. The bytes live at
/// [`media_key`]; `bytes`/`content_type`/`uploaded_at` are copied from the
/// blob's sidecar ([`ArchivedMediaMeta`]), which is the authoritative record
/// that the blob exists in the archive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaEntry {
    pub media_id: Uuid,
    /// `"image"` or `"video"`.
    pub kind: String,
    pub content_type: String,
    pub bytes: u64,
    pub object_key: String,
    pub uploaded_at: NaiveDateTime,
}

/// Sidecar record stored next to each written-through media blob
/// (`{media_key}.meta.json`). It carries the content type and original
/// filename so a read-through fetch can set response headers **before** the
/// session's manifest exists (write-through happens at upload time; the
/// manifest is only written at the much-later archive sweep). Writing the
/// bytes first and the sidecar last means "sidecar present" implies
/// "bytes complete" — the same ordering invariant as transcript-then-manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArchivedMediaMeta {
    pub media_id: Uuid,
    pub kind: String,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub bytes: u64,
    pub uploaded_at: NaiveDateTime,
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

/// Key for a media blob's raw bytes. Co-located under the session prefix (like
/// the manifest/transcript) so a session's whole archive lives at one prefix.
/// `media_id` is the served id from `/api/images/{id}` or `/api/media/{id}`.
pub fn media_key(user_id: Uuid, session_id: Uuid, media_id: Uuid) -> String {
    format!("v1/users/{user_id}/sessions/{session_id}/media/{media_id}")
}

/// Key for a media blob's JSON sidecar ([`ArchivedMediaMeta`]).
pub fn media_meta_key(user_id: Uuid, session_id: Uuid, media_id: Uuid) -> String {
    format!("v1/users/{user_id}/sessions/{session_id}/media/{media_id}.meta.json")
}

// ---------------------------------------------------------------------------
// Transcript merge / parse
// ---------------------------------------------------------------------------

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

/// Parse an uncompressed NDJSON transcript body into typed lines.
pub fn parse_transcript_ndjson(ndjson: &[u8]) -> std::io::Result<Vec<ArchiveMessageLine>> {
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

    pub(crate) fn manifest(user_id: Uuid, session_id: Uuid) -> SessionArchiveManifest {
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

    pub(crate) fn clone_line(l: &ArchiveMessageLine) -> ArchiveMessageLine {
        ArchiveMessageLine {
            id: l.id,
            role: l.role.clone(),
            created_at: l.created_at,
            agent_type: l.agent_type.clone(),
            content: l.content.clone(),
        }
    }

    #[test]
    fn zstd_transcript_roundtrips() {
        let body = b"{\"id\":1}\n{\"id\":2}\n";
        let encoded = zstd_encode(body).unwrap();
        assert_eq!(&encoded[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic bytes");
        assert_eq!(zstd_decode(&encoded).unwrap(), body);
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
}

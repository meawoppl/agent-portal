//! End-to-end tests over a real fixture archive built in a tempdir through
//! `archive-format`'s own put paths (`put_session_archive` / `put_media`), so
//! the viewer is exercised against genuinely-written objects rather than
//! hand-mocked rows. Covers multi-user / multi-session, media sidecars, and
//! one media-less older-style manifest.

use archive_format::{
    ArchiveMessageLine, ArchiveStore, ArchivedMediaMeta, LocalArchiveStore, SessionArchiveBundle,
};
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use crate::export::{self, Format};
use crate::rollup::{self, GroupBy};
use crate::rows::test_support::{manifest, media_entry};
use crate::rows::{collect_rows, filter_and_sort, resolve_session, Filters};
use crate::{cat, list};

const USER_A: u128 = 0xAAAA_0000_0000_0000_0000_0000_0000_000A;
const USER_B: u128 = 0xBBBB_0000_0000_0000_0000_0000_0000_000B;
const SESSION_A1: u128 = 0x1111_0000_0000_0000_0000_0000_0000_0001;
const SESSION_A2: u128 = 0x2222_0000_0000_0000_0000_0000_0000_0002;
const SESSION_B1: u128 = 0x3333_0000_0000_0000_0000_0000_0000_0003;

fn day(d: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 7, d)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

fn ndjson(lines: &[ArchiveMessageLine]) -> Vec<u8> {
    let mut buf = Vec::new();
    for l in lines {
        buf.extend_from_slice(serde_json::to_string(l).unwrap().as_bytes());
        buf.push(b'\n');
    }
    buf
}

fn msg(role: &str, day_of_month: u32, content: serde_json::Value) -> ArchiveMessageLine {
    ArchiveMessageLine {
        id: Uuid::new_v4(),
        role: role.to_string(),
        created_at: day(day_of_month),
        agent_type: "claude".to_string(),
        content,
    }
}

/// Build the fixture archive and return the tempdir (kept alive) + store.
fn build_fixture() -> (TempDir, ArchiveStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ArchiveStore::Local(LocalArchiveStore::new(dir.path().to_path_buf()));

    let (user_a, user_b) = (Uuid::from_u128(USER_A), Uuid::from_u128(USER_B));

    // --- User A, session 1: claude, media in manifest + written-through. ---
    let mut a1 = manifest(
        user_a,
        Uuid::from_u128(SESSION_A1),
        "alice@example.com",
        "refactor the rail",
        "claude",
        day(14),
    );
    a1.message_counts
        .extend([("user".to_string(), 2), ("assistant".to_string(), 1)]);
    a1.tokens.input = 100;
    a1.tokens.output = 50;
    a1.tokens.cache_creation = 10;
    a1.tokens.cache_read = 5;
    a1.tokens.thinking = 3;
    a1.total_cost_usd = 0.10;
    a1.turns.count = 2;
    a1.turns.tool_calls = 4;
    a1.turns.models = vec!["opus".to_string()];
    let media_id = Uuid::new_v4();
    a1.media = Some(vec![media_entry(media_id, 2048)]);
    store
        .put_session_archive(&SessionArchiveBundle {
            manifest: a1,
            transcript_ndjson: Some(ndjson(&[
                msg("user", 14, json!("please refactor the rail")),
                msg(
                    "assistant",
                    14,
                    json!({"content": [
                        {"type": "thinking", "thinking": "hidden reasoning"},
                        {"type": "text", "text": "starting the refactor"},
                        {"type": "tool_use", "name": "Edit", "input": {}},
                    ]}),
                ),
                msg("user", 14, json!("thanks")),
            ])),
        })
        .unwrap();
    let meta = ArchivedMediaMeta {
        media_id,
        kind: "image".to_string(),
        content_type: "image/png".to_string(),
        filename: Some("plot.png".to_string()),
        bytes: 2048,
        uploaded_at: day(14),
    };
    store
        .put_media(user_a, Uuid::from_u128(SESSION_A1), &meta, &vec![0u8; 2048])
        .unwrap();

    // --- User A, session 2: media-less, older-style manifest (media = None). ---
    let mut a2 = manifest(
        user_a,
        Uuid::from_u128(SESSION_A2),
        "alice@example.com",
        "docs pass",
        "claude",
        day(12),
    );
    a2.message_counts.insert("user".to_string(), 1);
    a2.tokens.input = 200;
    a2.tokens.output = 100;
    a2.total_cost_usd = 0.20;
    a2.turns.count = 1;
    a2.turns.tool_calls = 1;
    a2.turns.models = vec!["sonnet".to_string()];
    assert!(a2.media.is_none(), "a2 is the media-less older-style row");
    store
        .put_session_archive(&SessionArchiveBundle {
            manifest: a2,
            transcript_ndjson: Some(ndjson(&[msg("user", 12, json!("update the docs"))])),
        })
        .unwrap();

    // --- User B, session 1: codex, media. ---
    let mut b1 = manifest(
        user_b,
        Uuid::from_u128(SESSION_B1),
        "bob@example.com",
        "codex spike",
        "codex",
        day(13),
    );
    b1.tokens.input = 300;
    b1.total_cost_usd = 0.30;
    b1.turns.count = 3;
    b1.turns.models = vec!["gpt-5".to_string()];
    b1.media = Some(vec![media_entry(Uuid::new_v4(), 4096)]);
    store
        .put_session_archive(&SessionArchiveBundle {
            manifest: b1,
            transcript_ndjson: Some(ndjson(&[msg("user", 13, json!("spike a codex flow"))])),
        })
        .unwrap();

    (dir, store)
}

#[test]
fn collects_all_three_sessions_across_two_users() {
    let (_dir, store) = build_fixture();
    let rows = collect_rows(&store).unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn list_filters_by_agent_and_user() {
    let (_dir, store) = build_fixture();
    let rows = collect_rows(&store).unwrap();

    let codex = filter_and_sort(
        collect_rows(&store).unwrap(),
        &Filters {
            agent: Some("codex".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].manifest.owner_email, "bob@example.com");

    let alice = filter_and_sort(
        rows,
        &Filters {
            user: Some("alice".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(alice.len(), 2);
    // Sorted by last_activity desc: the day-14 session comes first.
    assert_eq!(alice[0].manifest.session_name, "refactor the rail");

    // The rendered table names the media-bearing and media-less rows.
    let out = list::render(&alice);
    assert!(out.contains("refactor the rail"));
    assert!(out.contains("docs pass"));
}

#[test]
fn rollup_by_user_sums_manifest_metrics() {
    let (_dir, store) = build_fixture();
    let rows = filter_and_sort(collect_rows(&store).unwrap(), &Filters::default());
    let out = rollup::render(&rows, GroupBy::User);

    let alice = out
        .lines()
        .find(|l| l.starts_with("alice@example.com"))
        .unwrap();
    // alice: 2 sessions, turns 2+1=3, input 100+200=300, output 50+100=150,
    // cache (10+5)+0=15, thinking 3, cost 0.30, tools 4+1=5, media 2048.
    assert!(alice.contains("300"), "input sum: {alice}");
    assert!(alice.contains("150"), "output sum: {alice}");
    assert!(alice.contains("$0.3000"), "cost sum: {alice}");
    assert!(alice.contains("2048"), "media bytes: {alice}");

    let bob = out
        .lines()
        .find(|l| l.starts_with("bob@example.com"))
        .unwrap();
    assert!(bob.contains("4096"), "bob media bytes: {bob}");
}

#[test]
fn export_csv_and_json_have_expected_shape() {
    let (_dir, store) = build_fixture();
    let rows = filter_and_sort(collect_rows(&store).unwrap(), &Filters::default());

    let csv = export::render(&rows, Format::Csv).unwrap();
    let csv_lines: Vec<&str> = csv.lines().collect();
    assert_eq!(csv_lines[0], export::CSV_HEADERS.join(","));
    assert_eq!(csv_lines.len(), 1 + 3, "header + 3 rows");
    assert!(csv.contains("alice@example.com"));
    assert!(csv.contains("opus"));

    let jsonv: serde_json::Value =
        serde_json::from_str(&export::render(&rows, Format::Json).unwrap()).unwrap();
    let arr = jsonv.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert!(arr.iter().any(|r| r["owner_email"] == "bob@example.com"));
    // The media-less A2 row reports zero media, provenance null.
    let a2 = arr
        .iter()
        .find(|r| r["session_name"] == "docs pass")
        .unwrap();
    assert_eq!(a2["media_count"], 0);
    assert!(a2["launcher_id"].is_null());
}

#[test]
fn cat_resolves_short_prefix_and_renders_digest() {
    let (_dir, store) = build_fixture();
    let rows = collect_rows(&store).unwrap();

    // "1111" uniquely identifies session A1.
    let row = resolve_session("1111", &rows).unwrap();
    assert_eq!(row.manifest.session_id, Uuid::from_u128(SESSION_A1));

    let digest = cat::run(&store, row.user_id, row.manifest.session_id, false).unwrap();
    let lines: Vec<&str> = digest.lines().collect();
    assert_eq!(lines.len(), 3, "one line per message");
    assert!(lines[0].contains("please refactor the rail"));
    assert!(lines[1].contains("starting the refactor"));
    assert!(lines[1].contains("[tool_use: Edit]"));
    // Thinking blocks are omitted from the digest.
    assert!(!digest.contains("hidden reasoning"));

    // Raw mode dumps the stored NDJSON verbatim (3 lines, valid JSON each).
    let raw = cat::run(&store, row.user_id, row.manifest.session_id, true).unwrap();
    assert_eq!(raw.lines().count(), 3);
    for l in raw.lines() {
        let _: serde_json::Value = serde_json::from_str(l).unwrap();
    }
}

#[test]
fn cat_missing_transcript_is_friendly() {
    let (_dir, store) = build_fixture();
    // A session id that was never archived -> resolve fails cleanly.
    let rows = collect_rows(&store).unwrap();
    assert!(resolve_session("ffffffff", &rows).is_err());
}

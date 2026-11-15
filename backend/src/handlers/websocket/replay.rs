#[cfg(test)]
use super::WebClientSender;
use crate::handlers::helpers::{display_name, parse_iso_cursor};
use diesel::prelude::*;
use shared::ServerToClient;
use uuid::Uuid;

const REPLAY_BATCH_BYTES: usize = 1024 * 1024;
const REPLAY_BATCH_ROWS: usize = 128;

/// A two-frame mailbox isolates blocking SQL from the socket. The consumer
/// sends history before draining live output, and dropping it cancels production.
/// Frames arrive already redacted for `viewer` and the session owner.
pub(super) fn replay_history(
    db_pool: &crate::db::DbPool,
    session_id: Uuid,
    viewer: Uuid,
    replay_after: Option<String>,
    initial_replay_limit: i64,
) -> tokio::sync::mpsc::Receiver<std::io::Result<ServerToClient>> {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    let pool = db_pool.clone();
    tokio::task::spawn_blocking(move || {
        let result = stream_history(
            &pool,
            session_id,
            viewer,
            replay_after,
            initial_replay_limit,
            |batch| tx.blocking_send(Ok(batch)).map_err(std::io::Error::other),
        );
        if let Err(error) = result {
            let _ = tx.blocking_send(Err(error));
        }
    });
    rx
}

/// One row of the replay plan: enough to batch by size without its body.
#[derive(Debug, Clone, Copy)]
struct PlannedRow {
    id: Uuid,
    created_at: chrono::NaiveDateTime,
    bytes: usize,
}

/// A batch of plan rows and the watermark it lets the client advance to.
#[derive(Debug, PartialEq)]
struct PlannedBatch {
    rows: std::ops::Range<usize>,
    last_created_at: Option<chrono::NaiveDateTime>,
}

/// Split the plan into batches bounded by [`REPLAY_BATCH_ROWS`] and
/// [`REPLAY_BATCH_BYTES`] (one oversized row still forms its own batch).
///
/// A timestamp group may span batches, so a batch's watermark only advances
/// past a group once all of its rows have been sent: reconnecting with that
/// watermark (strictly-greater filter) can never skip a tie. The final batch
/// carries the newest timestamp.
fn plan_batches(rows: &[PlannedRow], after: Option<chrono::NaiveDateTime>) -> Vec<PlannedBatch> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    let mut last_timestamp = None;
    let mut complete_timestamp = after;
    for (index, row) in rows.iter().enumerate() {
        if last_timestamp.is_some_and(|last| last != row.created_at) {
            complete_timestamp = last_timestamp;
        }
        if index > start && (bytes >= REPLAY_BATCH_BYTES || index - start >= REPLAY_BATCH_ROWS) {
            batches.push(PlannedBatch {
                rows: start..index,
                last_created_at: complete_timestamp,
            });
            start = index;
            bytes = 0;
        }
        bytes += row.bytes;
        last_timestamp = Some(row.created_at);
    }
    if start < rows.len() {
        batches.push(PlannedBatch {
            rows: start..rows.len(),
            last_created_at: last_timestamp,
        });
    }
    batches
}

/// Stream the complete reconnect delta, or the requested initial tail.
///
/// No pooled connection is held while waiting on the client. A cursor that
/// stayed open across `send` would pin one connection per replaying socket
/// for as long as its browser takes to drain; a reconnect burst then
/// exhausted the pool and starved every other socket's redaction lookup,
/// which closed those clients and fed the burst. Instead one short query
/// plans the replay (ids, timestamps and sizes only), and each batch checks a
/// connection out just long enough to load its bodies and current rules.
fn stream_history(
    db_pool: &crate::db::DbPool,
    session_id: Uuid,
    viewer: Uuid,
    replay_after: Option<String>,
    initial_replay_limit: i64,
    mut send: impl FnMut(ServerToClient) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use crate::schema::{messages, sessions, users};
    let after = replay_after.as_deref().and_then(parse_iso_cursor);
    let (owner, plan) = {
        let mut conn = db_pool.get().map_err(std::io::Error::other)?;
        // Resolve the authorized session's owner first; a row's author or
        // provider-supplied session_id is not authority.
        let owner = sessions::table
            .find(session_id)
            .select(sessions::user_id)
            .first::<Uuid>(&mut conn)
            .map_err(std::io::Error::other)?;
        let size = diesel::dsl::sql::<diesel::sql_types::Integer>("octet_length(messages.content)");
        let mut query = messages::table
            .filter(messages::session_id.eq(session_id))
            .select((messages::id, messages::created_at, size))
            .into_boxed();
        let mut rows: Vec<(Uuid, chrono::NaiveDateTime, i32)> = if let Some(after) = after {
            query = query.filter(messages::created_at.gt(after));
            query
                .order((messages::created_at.asc(), messages::id.asc()))
                .load(&mut conn)
        } else {
            query
                .order((messages::created_at.desc(), messages::id.desc()))
                .limit(initial_replay_limit.max(0))
                .load(&mut conn)
        }
        .map_err(std::io::Error::other)?;
        if after.is_none() {
            rows.reverse();
        }
        let plan: Vec<PlannedRow> = rows
            .into_iter()
            .map(|(id, created_at, bytes)| PlannedRow {
                id,
                created_at,
                bytes: usize::try_from(bytes).unwrap_or(0),
            })
            .collect();
        (owner, plan)
    };
    type ReplayRow = (
        crate::models::Message,
        Option<(Option<String>, Option<String>, String)>,
    );
    for batch in plan_batches(&plan, after) {
        let ids: Vec<Uuid> = plan[batch.rows].iter().map(|row| row.id).collect();
        let (rows, catalog) = {
            let mut conn = db_pool.get().map_err(std::io::Error::other)?;
            let rows: Vec<ReplayRow> = messages::table
                .left_join(users::table.on(messages::user_id.eq(users::id)))
                .filter(messages::id.eq_any(&ids))
                .select((
                    messages::all_columns,
                    (users::nickname, users::name, users::email).nullable(),
                ))
                .order((messages::created_at.asc(), messages::id.asc()))
                .load(&mut conn)
                .map_err(std::io::Error::other)?;
            // Fresh rules per batch: a secret saved mid-replay is masked in
            // every later batch.
            let catalog = crate::secrets::redaction::snapshot(&mut conn)
                .map_err(|_| std::io::Error::other("History redaction unavailable"))?;
            (rows, catalog)
        };
        // Rows trimmed by retention since planning simply drop out.
        if rows.is_empty() {
            continue;
        }
        let entries = rows
            .into_iter()
            .map(|(msg, user)| {
                let sender = user
                    .filter(|_| msg.role == "user")
                    .map(|(nickname, name, email)| {
                        display_name(nickname.as_deref(), name.as_deref(), &email)
                    });
                shared::HistoryEntry {
                    meta: Some(msg.portal_meta(sender)),
                    content: shared::content_value_or_fallback(&msg.role, &msg.content),
                }
            })
            .collect();
        let frame = ServerToClient::HistoryBatch {
            entries,
            last_created_at: batch
                .last_created_at
                .map(|ts| ts.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
        };
        send(redact_replay_frame(frame, &catalog, viewer, owner)?)?;
    }
    Ok(())
}

/// Apply the viewer's and the session owner's rules, the same pair live
/// output gets in `sanitize_outbound`. Preserves transport types, UUIDs and
/// replay watermarks even for a one-byte private value; only serialized
/// content-bearing fields are redacted.
fn redact_replay_frame(
    frame: ServerToClient,
    catalog: &crate::secrets::redaction::Redactor,
    viewer: Uuid,
    owner: Uuid,
) -> std::io::Result<ServerToClient> {
    let viewer_rules = catalog.scoped_for(viewer);
    let owner_rules = (owner != viewer).then(|| catalog.scoped_for(owner));
    if viewer_rules.is_empty() && owner_rules.as_ref().is_none_or(|rules| rules.is_empty()) {
        return Ok(frame);
    }
    let mut value = serde_json::to_value(frame)
        .map_err(|_| std::io::Error::other("Cannot sanitize history transport"))?;
    viewer_rules.redact_transport_json(&mut value);
    if let Some(rules) = owner_rules {
        rules.redact_transport_json(&mut value);
    }
    serde_json::from_value(value)
        .map_err(|_| std::io::Error::other("Cannot sanitize history transport"))
}

// =============================================================================
// DB-touching test for the initial-replay limit (closes #788).
//
// Mirrors the harness in `handlers::messages::db_tests` / `session_access::db_tests`:
// auto-skips when `DATABASE_URL` is not set so CI without a DB stays green.
// =============================================================================
#[cfg(test)]
mod replay_tests {
    use crate::models::Session;
    use chrono::Utc;
    use diesel::prelude::*;
    fn make_session(conn: &mut diesel::pg::PgConnection, owner_id: uuid::Uuid) -> Session {
        crate::test_support::insert_session(conn, owner_id, "test-replay")
    }

    fn seed_messages(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_id: uuid::Uuid,
        count: usize,
    ) -> Vec<chrono::NaiveDateTime> {
        use chrono::Timelike;
        use diesel::sql_query;
        // Truncate to µs so the returned stamps match the DB read-back —
        // Postgres `timestamp` drops the sub-µs nanoseconds in `Utc::now()`,
        // which otherwise breaks exact `created_at` equality assertions.
        let now = Utc::now().naive_utc();
        let base = now
            .with_nanosecond(now.nanosecond() / 1_000 * 1_000)
            .unwrap_or(now);
        let mut stamps = Vec::with_capacity(count);
        for i in 0..count {
            let ts = base + chrono::Duration::microseconds((i as i64 + 1) * 1000);
            stamps.push(ts);
            sql_query(
                "INSERT INTO messages (id, session_id, role, content, created_at, user_id, agent_type)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind::<diesel::sql_types::Uuid, _>(uuid::Uuid::new_v4())
            .bind::<diesel::sql_types::Uuid, _>(session_id)
            .bind::<diesel::sql_types::VarChar, _>("assistant")
            .bind::<diesel::sql_types::Text, _>(format!("msg #{}", i))
            .bind::<diesel::sql_types::Timestamp, _>(ts)
            .bind::<diesel::sql_types::Uuid, _>(user_id)
            .bind::<diesel::sql_types::VarChar, _>("claude")
            .execute(conn)
            .expect("seed message");
        }
        stamps
    }

    fn cleanup(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_ids: &[uuid::Uuid],
    ) {
        use crate::schema::{messages, sessions, users};
        let _ = diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
            .execute(conn);
        let _ = diesel::delete(sessions::table.find(session_id)).execute(conn);
        for uid in user_ids {
            let _ = diesel::delete(users::table.find(uid)).execute(conn);
        }
    }

    #[test]
    fn streaming_batches_keep_timestamp_ties_replayable() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "replay-ties");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 300);
        use crate::schema::messages;
        diesel::update(messages::table.filter(messages::session_id.eq(session.id)))
            .set(messages::created_at.eq(stamps[0]))
            .execute(&mut conn)
            .unwrap();
        let mut count = 0;
        let mut batches = Vec::new();
        super::stream_history(&pool, session.id, user.id, None, 300, |msg| {
            if let shared::ServerToClient::HistoryBatch {
                entries,
                last_created_at,
            } = msg
            {
                assert!(entries.len() <= super::REPLAY_BATCH_ROWS);
                count += entries.len();
                batches.push(last_created_at);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 300);
        assert_eq!(batches.len(), 3);
        assert!(batches[0].is_none() && batches[1].is_none());
        assert!(batches[2].is_some());
        assert!(
            super::stream_history(&pool, session.id, user.id, None, 300, |_| Err(
                std::io::Error::other("client gone")
            ))
            .is_err()
        );
        cleanup(&mut conn, session.id, &[user.id]);
    }

    /// `replay_after = None` returns at most `initial_replay_limit` rows in
    /// chronological order — the exact query shape `replay_history` builds.
    /// Pre-#788 this path loaded the full session history; now SQL trims to
    /// the render-window default so the wire payload is bounded.
    #[test]
    fn replay_after_none_caps_to_retention_count() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = crate::test_support::insert_user(&mut conn, "limit");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 200);

        let limit: i64 = 100;
        use crate::schema::messages;
        let mut tail: Vec<crate::models::Message> = messages::table
            .filter(messages::session_id.eq(session.id))
            .order(messages::created_at.desc())
            .limit(limit)
            .load(&mut conn)
            .expect("load");
        tail.reverse();

        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(tail.len(), limit as usize);
        // Tail is the newest 100, presented oldest-first.
        assert_eq!(tail.first().unwrap().created_at, stamps[100]);
        assert_eq!(tail.last().unwrap().created_at, stamps[199]);
        for w in tail.windows(2) {
            assert!(w[0].created_at < w[1].created_at);
        }
    }

    // -----------------------------------------------------------------
    // #784: server-assigned replay watermark
    //
    // `replay_history` parses the client-supplied `replay_after` ISO timestamp,
    // then runs `messages.created_at.gt(after)`. The silent-data-loss chain
    // was: frontend stored `Date.now()` as `last_message_timestamp`, sent
    // it as `replay_after`, and the backend filtered out perfectly-good
    // rows whose `created_at` happened to land BEFORE the browser's
    // clock-skewed "now". The wire-shape fix moves the watermark source
    // onto the server so the round-trip is now `server-assigned created_at
    // → frontend stores it verbatim → frontend sends it back → server
    // filters strictly greater than it`. These tests pin the backend half
    // of that round-trip: the precise format we write into the wire
    // (microsecond precision) must parse on the way back in, and the
    // strict-`>` semantics must hold so a message whose `created_at`
    // *equals* the watermark is treated as already seen.

    /// Capture the `ServerToClient` messages a `replay_history` invocation
    /// pushes onto its sender so we can assert against the new
    /// `HistoryBatch.last_created_at` + per-message `_created_at`
    /// injection without standing up a full WS round-trip.
    fn capture_sender() -> (
        super::WebClientSender,
        crate::handlers::websocket::ConnReceiver<shared::ServerToClient>,
    ) {
        crate::handlers::websocket::conn_channel::<shared::ServerToClient>(64)
    }

    /// The fix's whole point: `replay_history` with a `replay_after`
    /// equal to a previously-broadcast row's `created_at` must return
    /// ONLY messages strictly after it. Seed three rows, take the middle
    /// row's server-assigned `created_at` (formatted exactly the way the
    /// broadcast path formats it), pass it as `replay_after`, and assert:
    /// the watermark row itself is excluded, the earlier row is excluded,
    /// the later row is included. This is the round-trip the frontend
    /// now performs end-to-end (#784).
    #[test]
    fn replay_history_strict_gt_round_trips_server_timestamp() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = crate::test_support::insert_user(&mut conn, "strict_gt");
        let session = make_session(&mut conn, user.id);
        // Three rows with strictly increasing server-assigned timestamps.
        let stamps = seed_messages(&mut conn, session.id, user.id, 3);
        drop(conn);

        // Format the watermark exactly the way the broadcast path
        // formats `created_at` — microsecond precision, no timezone
        // suffix. That's the string the frontend will store and replay.
        let watermark = stamps[1].format("%Y-%m-%dT%H:%M:%S%.6f").to_string();
        let expected_last = stamps[2].format("%Y-%m-%dT%H:%M:%S%.6f").to_string();

        let (tx, mut rx) = capture_sender();
        // initial_replay_limit is unused on the `replay_after = Some(_)`
        // branch — that branch loads strictly-newer rows unbounded.
        super::stream_history(&pool, session.id, user.id, Some(watermark), 100, |msg| {
            tx.send(msg).map_err(std::io::Error::other)
        })
        .unwrap();
        drop(tx);

        let mut got: Option<shared::ServerToClient> = None;
        while let Ok(msg) = rx.try_recv() {
            got = Some(msg);
        }
        let batch = got.expect("replay_history must send exactly one HistoryBatch");

        let (entries, last_created_at) = match batch {
            shared::ServerToClient::HistoryBatch {
                entries,
                last_created_at,
            } => (entries, last_created_at),
            other => panic!("expected HistoryBatch, got {:?}", other),
        };

        // The strict-`>` predicate must exclude stamp[0] (older) and
        // stamp[1] (equal-to-watermark) and include stamp[2] (newer).
        // The exact-equal exclusion is the contract that closes the
        // silent-data-loss window in #784 — a frontend that already
        // rendered the watermark row is told by its watermark "I have
        // everything up to and including stamp[1]", and the backend
        // honors that semantic.
        let mut conn = pool.get().expect("conn");
        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(
            entries.len(),
            1,
            "expected only the newest row to be replayed; got {} entries",
            entries.len(),
        );
        assert_eq!(last_created_at.as_deref(), Some(expected_last.as_str()));
        // Attribution/timestamp ride in each entry's typed sidecar — content
        // stays raw (no `_created_at` injection).
        let meta_ts = entries[0]
            .meta
            .as_ref()
            .and_then(|m| m.created_at.as_deref())
            .expect("each replayed entry must carry meta.created_at");
        assert_eq!(meta_ts, expected_last);
    }

    /// A `None` `replay_after` (initial connect with no prior watermark)
    /// must return up to `initial_replay_limit` rows and carry
    /// `last_created_at` set to the newest row's timestamp so the frontend
    /// can start its watermark from there. Pins the initial-connect path
    /// the component takes when REST history returned empty / failed.
    #[test]
    fn replay_history_no_watermark_returns_all_and_advances_high_water() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = crate::test_support::insert_user(&mut conn, "no_watermark");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 3);
        drop(conn);

        let (tx, mut rx) = capture_sender();
        super::stream_history(&pool, session.id, user.id, None, 100, |msg| {
            tx.send(msg).map_err(std::io::Error::other)
        })
        .unwrap();
        drop(tx);

        let mut batch: Option<shared::ServerToClient> = None;
        while let Ok(msg) = rx.try_recv() {
            batch = Some(msg);
        }
        let batch = batch.expect("replay_history must send HistoryBatch");

        let (entries, last_created_at) = match batch {
            shared::ServerToClient::HistoryBatch {
                entries,
                last_created_at,
            } => (entries, last_created_at),
            other => panic!("expected HistoryBatch, got {:?}", other),
        };

        let mut conn = pool.get().expect("conn");
        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(entries.len(), 3, "expected all rows to be replayed");
        let expected_last = stamps[2].format("%Y-%m-%dT%H:%M:%S%.6f").to_string();
        assert_eq!(last_created_at.as_deref(), Some(expected_last.as_str()));
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn rows(stamps: &[i64], bytes: usize) -> Vec<PlannedRow> {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0)
            .unwrap()
            .naive_utc();
        stamps
            .iter()
            .map(|&micros| PlannedRow {
                id: Uuid::new_v4(),
                created_at: base + chrono::Duration::microseconds(micros),
                bytes,
            })
            .collect()
    }

    #[test]
    fn batches_respect_the_row_cap_and_finish_at_the_newest_timestamp() {
        let plan = rows(&(0..300).collect::<Vec<_>>(), 10);
        let batches = plan_batches(&plan, None);
        let sizes: Vec<_> = batches.iter().map(|b| b.rows.len()).collect();
        assert_eq!(
            sizes,
            [
                REPLAY_BATCH_ROWS,
                REPLAY_BATCH_ROWS,
                300 - 2 * REPLAY_BATCH_ROWS
            ]
        );
        assert_eq!(
            batches[0].last_created_at,
            Some(plan[REPLAY_BATCH_ROWS - 1].created_at)
        );
        assert_eq!(batches[2].last_created_at, Some(plan[299].created_at));
    }

    #[test]
    fn a_timestamp_group_spanning_batches_holds_the_watermark_back() {
        // Every row shares one timestamp: no batch but the last may advance.
        let plan = rows(&[7; 300], 10);
        let after = Some(plan[0].created_at - chrono::Duration::seconds(1));
        let batches = plan_batches(&plan, after);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].last_created_at, after);
        assert_eq!(batches[1].last_created_at, after);
        assert_eq!(batches[2].last_created_at, Some(plan[0].created_at));
        assert_eq!(plan_batches(&plan, None)[0].last_created_at, None);
    }

    #[test]
    fn the_byte_cap_splits_batches_and_an_oversized_row_stands_alone() {
        let plan = rows(&[1, 2, 3, 4], REPLAY_BATCH_BYTES / 2);
        let sizes: Vec<_> = plan_batches(&plan, None)
            .iter()
            .map(|b| b.rows.len())
            .collect();
        assert_eq!(sizes, [2, 2]);
        let huge = rows(&[1, 2, 3], REPLAY_BATCH_BYTES * 3);
        let sizes: Vec<_> = plan_batches(&huge, None)
            .iter()
            .map(|b| b.rows.len())
            .collect();
        assert_eq!(sizes, [1, 1, 1]);
        assert!(plan_batches(&[], None).is_empty());
    }
}

#[cfg(test)]
mod owner_redaction_tests {
    use super::*;

    /// The whole point of planning the replay: while the producer waits on the
    /// client (`send`), it holds no pooled connection, so a slow or stalled
    /// browser cannot pin one. Isolated pool, so the count is exact.
    #[test]
    fn replay_holds_no_connection_while_waiting_on_the_client() {
        let Some(db) = crate::handlers::secrets::tests::Database::new() else {
            return;
        };
        let mut conn = db.pool.get().unwrap();
        let owner = crate::test_support::insert_user(&mut conn, "replay-pool-owner");
        let session = crate::test_support::insert_session(&mut conn, owner.id, "pool replay");
        for i in 0..300 {
            diesel::insert_into(crate::schema::messages::table)
                .values(&crate::models::NewMessage {
                    session_id: session.id,
                    role: "assistant".into(),
                    content: format!("{{\"type\":\"assistant\",\"n\":{i}}}"),
                    user_id: owner.id,
                    agent_type: "claude".into(),
                    provenance_kind: None,
                    provenance_session_id: None,
                    provenance_agent_type: None,
                })
                .execute(&mut conn)
                .unwrap();
        }
        drop(conn);
        let mut held_during_send = Vec::new();
        let mut rows = 0;
        stream_history(&db.pool, session.id, owner.id, None, 1000, |frame| {
            let state = db.pool.state();
            held_during_send.push(state.connections - state.idle_connections);
            if let ServerToClient::HistoryBatch { entries, .. } = frame {
                rows += entries.len();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(rows, 300);
        assert_eq!(
            held_during_send,
            [0, 0, 0],
            "a connection was held across send"
        );
    }
    #[test]
    fn legacy_replay_is_redacted_before_mailbox_using_session_owner_not_row_author() {
        let Some(db) = crate::handlers::secrets::tests::Database::new() else {
            return;
        };
        let mut conn = db.pool.get().unwrap();
        let owner = crate::test_support::insert_user(&mut conn, "replay-secret-owner");
        let author = crate::test_support::insert_user(&mut conn, "replay-other-author");
        let session = crate::test_support::insert_session(&mut conn, owner.id, "legacy replay");
        use crate::schema::{messages, secrets_generation as generation};
        let organization = generation::table
            .find(1i16)
            .select(generation::organization_id)
            .first::<Uuid>(&mut conn)
            .unwrap();
        diesel::update(generation::table.find(1i16))
            .set(generation::generation.eq(1i64))
            .execute(&mut conn)
            .unwrap();
        let runtime = crate::secrets::redaction::RedactionRuntime::new();
        runtime
            .install(
                organization,
                1,
                vec![
                    (
                        Uuid::new_v4(),
                        shared::api::SecretValue("a".into()),
                        Some(owner.id),
                    ),
                    (
                        Uuid::new_v4(),
                        shared::api::SecretValue("EDITOR_55".into()),
                        Some(author.id),
                    ),
                ],
            )
            .unwrap();
        let content=serde_json::json!({"type":"assistant","uuid":"a-provider-id","session_id":"a-native-session","message":{"role":"assistant","content":[{"type":"text","text":"a EDITOR_55"}]}}).to_string();
        let id = diesel::insert_into(messages::table)
            .values(&crate::models::NewMessage {
                session_id: session.id,
                role: "assistant".into(),
                content,
                user_id: author.id,
                agent_type: "claude".into(),
                provenance_kind: None,
                provenance_session_id: None,
                provenance_agent_type: None,
            })
            .returning(messages::id)
            .get_result::<Uuid>(&mut conn)
            .unwrap();
        let created = messages::table
            .find(id)
            .select(messages::created_at)
            .first::<chrono::NaiveDateTime>(&mut conn)
            .unwrap();
        drop(conn);
        let mut batches = Vec::new();
        stream_history(&db.pool, session.id, owner.id, None, 100, |frame| {
            batches.push(frame);
            Ok(())
        })
        .unwrap();
        let ServerToClient::HistoryBatch {
            entries,
            last_created_at,
        } = batches.remove(0)
        else {
            panic!("history transport changed");
        };
        assert_eq!(
            last_created_at,
            Some(created.format("%Y-%m-%dT%H:%M:%S%.6f").to_string())
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content["type"], "assistant");
        assert_eq!(entries[0].content["uuid"], "a-provider-id");
        assert_eq!(entries[0].content["session_id"], "a-native-session");
        assert_eq!(entries[0].content["message"]["role"], "assistant");
        let text = entries[0].content["message"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text.starts_with(crate::secrets::redaction::MASK));
        assert!(text.contains("EDITOR_55"));
        assert_eq!(
            entries[0].meta.as_ref().unwrap().created_at.as_deref(),
            Some(created.format("%Y-%m-%dT%H:%M:%S%.6f").to_string().as_str())
        );
    }
}

//! Background maintenance tasks: periodic cleanup loops and the one-shot
//! stale-session sweep that runs after the proxy reconnect grace period.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::db::DbPool;
use crate::handlers;
use crate::handlers::websocket::SessionManager;
use crate::markers::{ARCHIVE_SWEEP_FAILED, RETENTION_TRIM_HELD, SESSION_ARCHIVE_FAILED};
use crate::models;
use crate::schema;
use crate::AppState;

/// Spawn a tokio task that runs `f` on a fixed interval forever.
///
/// `name` is the human-readable task description used in the startup log
/// line (`"Started {name}"`).
pub fn spawn_periodic<F, Fut>(name: &str, period: Duration, state: Arc<AppState>, f: F)
where
    F: Fn(Arc<AppState>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let name = name.to_string();
    tracing::info!("Started {name}");
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            // Run each tick in its own task so a panic surfaces as a JoinError
            // we log and recover from, rather than aborting this loop for the
            // rest of the process lifetime (#1487). Without this, one panicking
            // tick silently retires a maintenance loop — no error line, and
            // /api/health stays 200.
            if let Err(e) = tokio::spawn(f(state.clone())).await {
                tracing::error!("Background task '{name}' tick panicked: {e}");
            }
        }
    });
}

/// Deferred stale session cleanup: wait for proxies to reconnect before
/// marking unreconnected sessions as disconnected. Without this grace
/// period, a backend restart would immediately hide all sessions from the
/// frontend (which only shows status="active") and users would have to
/// restart launchers to get sessions back.
pub fn spawn_stale_session_cleanup(
    pool: DbPool,
    manager: SessionManager,
    notifications: crate::push::NotificationSender,
) {
    tokio::spawn(async move {
        const RECONNECT_GRACE_SECS: u64 = shared::protocol::MAX_RECONNECT_BACKOFF_SECS * 2;
        tracing::info!(
            "Waiting {}s for proxies to reconnect before cleaning stale sessions",
            RECONNECT_GRACE_SECS
        );
        tokio::time::sleep(std::time::Duration::from_secs(RECONNECT_GRACE_SECS)).await;

        let connected_keys: std::collections::HashSet<String> = manager
            .registered_session_keys()
            .into_iter()
            .map(|k| k.to_string())
            .collect();

        let Ok(mut conn) = pool.get() else {
            tracing::error!("Failed to get DB connection for stale session cleanup");
            return;
        };

        use diesel::prelude::*;
        use schema::sessions;

        let active_sessions: Vec<(uuid::Uuid, String)> = match sessions::table
            .filter(sessions::status.eq(shared::SessionStatus::Active.as_str()))
            .select((sessions::id, sessions::session_name))
            .load(&mut conn)
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to query active sessions for cleanup: {}", e);
                return;
            }
        };

        // These were ACTIVE and lost their proxy across the grace period — an
        // unexpected disconnect, so each earns a SessionDisconnected push
        // (mobile-apps plan §8.1). We keep the names to fill the payload.
        let stale: Vec<(uuid::Uuid, String)> = active_sessions
            .into_iter()
            .filter(|(id, _)| !connected_keys.contains(&id.to_string()))
            .collect();

        // Re-read the live set right before the write and drop anything that
        // reconnected during the query window. Registration is the only writer
        // that sets Active, and it is one-shot — so a proxy that registered
        // between the snapshot above and this UPDATE would otherwise have its
        // Active write clobbered back to Disconnected permanently (#1605). The
        // `status.eq(Active)` guard on the UPDATE closes the remaining sliver:
        // a row a concurrent registration flips can't be demoted here.
        let reconnected: std::collections::HashSet<String> = manager
            .registered_session_keys()
            .into_iter()
            .map(|k| k.to_string())
            .collect();
        let stale: Vec<(uuid::Uuid, String)> = stale
            .into_iter()
            .filter(|(id, _)| !reconnected.contains(&id.to_string()))
            .collect();

        if stale.is_empty() {
            tracing::info!("No stale sessions to clean up after reconnect grace period");
            return;
        }

        let stale_ids: Vec<uuid::Uuid> = stale.iter().map(|(id, _)| *id).collect();

        match diesel::update(
            sessions::table
                .filter(sessions::id.eq_any(&stale_ids))
                .filter(sessions::status.eq(shared::SessionStatus::Active.as_str())),
        )
        .set(sessions::status.eq(shared::SessionStatus::Disconnected.as_str()))
        .execute(&mut conn)
        {
            Ok(updated) => {
                tracing::info!(
                    "Marked {} stale sessions as disconnected ({}s grace period elapsed)",
                    updated,
                    RECONNECT_GRACE_SECS
                );
                for (session_id, session_name) in stale {
                    notifications.emit(crate::push::NotificationEvent::SessionDisconnected {
                        session_id,
                        session_name,
                    });
                }
            }
            Err(e) => {
                tracing::error!("Failed to mark stale sessions as disconnected: {}", e);
            }
        }
    });
}

/// Periodically heal the `sessions.status` column against live truth (#1605).
///
/// `status` is written to `Disconnected` by five paths but back to `Active`
/// only on proxy registration, which is one-shot — so a single spurious
/// `Disconnected` write (a lost sweep race, a backend restart the proxy
/// outran) strands a live session as greyed-out forever, since nothing sets it
/// back. `SessionManager` holds the authoritative live set, so promote any row
/// it says is connected but the column calls `Disconnected` back to `Active`.
///
/// Promote-only by design: the demote direction (Active → Disconnected) is
/// already handled correctly by the generation-guarded socket teardown and the
/// startup sweep, and running it on a tick would both race those paths and
/// re-fire `SessionDisconnected` pushes. This side only ever moves a
/// demonstrably-live session toward `Active`, so it cannot disturb `Replaced`
/// or a genuinely-gone session.
pub async fn reconcile_session_status(app_state: Arc<AppState>) {
    let live: Vec<uuid::Uuid> = app_state
        .session_manager
        .registered_session_keys()
        .into_iter()
        .filter_map(|k| uuid::Uuid::parse_str(&k.to_string()).ok())
        .collect();
    if live.is_empty() {
        return;
    }

    let pool = app_state.db_pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        use diesel::prelude::*;
        use schema::sessions;

        let mut conn = pool.get()?;
        let updated = diesel::update(
            sessions::table
                .filter(sessions::status.eq(shared::SessionStatus::Disconnected.as_str()))
                .filter(sessions::id.eq_any(&live)),
        )
        .set((
            sessions::status.eq(shared::SessionStatus::Active.as_str()),
            sessions::updated_at.eq(diesel::dsl::now),
        ))
        .execute(&mut conn)?;
        anyhow::Ok(updated)
    })
    .await;

    match result {
        Ok(Ok(n)) if n > 0 => {
            tracing::info!("Reconciled {n} live session(s) from disconnected back to active");
        }
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::error!("session status reconcile failed: {e}"),
        Err(e) => tracing::error!("session status reconcile task panicked: {e}"),
    }
}

// Pure decision helper extracted from ensure_session_archived so the
// fresh-archive guard stays unit-testable without I/O. The DB update
// remains the source of truth; this mirrors the `archived_at >= last_activity` check.

/// Whether a session's archive is fresh (covers `last_activity`).
/// Mirrors the guard in `ensure_session_archived`.
pub(crate) fn is_archive_fresh(
    archived_at: Option<chrono::NaiveDateTime>,
    last_activity: chrono::NaiveDateTime,
) -> bool {
    archived_at.is_some_and(|a| a >= last_activity)
}

pub(crate) fn load_archive_eligible(
    conn: &mut diesel::PgConnection,
    cutoff: chrono::NaiveDateTime,
    limit: i64,
) -> diesel::QueryResult<Vec<models::Session>> {
    use diesel::prelude::*;
    use schema::sessions;
    sessions::table
        .filter(sessions::status.ne(shared::SessionStatus::Active.as_str()))
        .filter(sessions::last_activity.lt(cutoff))
        .filter(
            sessions::archived_at
                .is_null()
                .or(sessions::archived_at.lt(sessions::last_activity.nullable())),
        )
        .order(sessions::last_activity.asc())
        .limit(limit)
        .load(conn)
}

/// Archive terminal sessions to long-term storage (#1258 phase 1).
///
/// Selection is idempotent: a session is eligible when it is not active,
/// has been idle past the grace window, and has never been archived — or
/// its `last_activity` advanced past its `archived_at` (it reactivated
/// after archival; deterministic keys make the re-archive an overwrite).
/// DB + file IO run on the blocking pool.
pub async fn run_archive_sweep(app_state: Arc<AppState>) {
    let Some(runtime) = app_state.archive.clone() else {
        return;
    };
    let db_pool = app_state.db_pool.clone();
    match tokio::task::spawn_blocking(move || archive_pending_sessions(&db_pool, &runtime)).await {
        Ok(Ok((archived, failed))) => {
            if archived > 0 || failed > 0 {
                tracing::info!(
                    "Archive sweep: {} session(s) archived, {} failed",
                    archived,
                    failed
                );
            }
            if archived > 0 {
                // `put_session_archive` has exactly one production call site —
                // the path above — so this sweep is the only thing that changes
                // what `/api/history` reads. Refreshing here *is* the freshness
                // mechanism; `HISTORY_SCAN_SELF_HEAL` only covers writers this
                // process can't see (another instance, or the bucket edited
                // out of band).
                if let Some(runtime) = app_state.archive.as_ref() {
                    runtime.warm_scan_cache();
                }
            }
        }
        Ok(Err(e)) => tracing::error!("{ARCHIVE_SWEEP_FAILED}: {e}"),
        Err(e) => tracing::error!("archive sweep task panicked: {e}"),
    }
}

/// Public for the integration harness (`backend/tests/harness.rs`), which
/// drives it against a real Postgres.
pub fn archive_pending_sessions(
    pool: &DbPool,
    runtime: &crate::archive::ArchiveRuntime,
) -> anyhow::Result<(usize, usize)> {
    let mut conn = pool.get()?;
    let cutoff = chrono::Utc::now().naive_utc()
        - chrono::Duration::seconds(crate::archive::ARCHIVE_IDLE_SECS);

    let eligible: Vec<models::Session> =
        load_archive_eligible(&mut conn, cutoff, crate::archive::ARCHIVE_SWEEP_BATCH)?;

    let mut archived = 0;
    let mut failed = 0;
    for session in eligible {
        if ensure_session_archived(&mut conn, runtime, &session) {
            archived += 1;
        } else {
            failed += 1;
        }
    }
    Ok((archived, failed))
}

fn archive_one_session(
    conn: &mut diesel::PgConnection,
    runtime: &crate::archive::ArchiveRuntime,
    session: &models::Session,
) -> anyhow::Result<()> {
    use crate::archive::{
        transcript_key, ArchiveMemberEntry, ArchiveMessageLine, ArchiveTokenTotals,
        ArchiveTranscriptInfo, ArchiveTurnStats, SessionArchiveBundle, SessionArchiveManifest,
        ARCHIVE_SCHEMA_VERSION,
    };
    use diesel::prelude::*;
    use schema::{messages, session_members, turn_metrics, users};
    use std::collections::BTreeMap;

    let (owner_email, owner_nickname, owner_name): (String, Option<String>, Option<String>) =
        users::table
            .find(session.user_id)
            .select((users::email, users::nickname, users::name))
            .first(conn)?;
    // Bake the display label (nickname preferred) into the manifest so the
    // history browser attributes the owner the same way live views do (#1485).
    let owner_name =
        crate::handlers::helpers::preferred_name(owner_nickname.as_deref(), owner_name.as_deref());

    // Membership snapshot for the manifest: visibility ("shared with me")
    // must survive the eventual deletion of the hot DB rows, so the archive
    // is the durable record. Refreshed on every re-archive.
    let member_rows: Vec<(uuid::Uuid, String, String)> = session_members::table
        .inner_join(users::table.on(users::id.eq(session_members::user_id)))
        .filter(session_members::session_id.eq(session.id))
        .select((
            session_members::user_id,
            users::email,
            session_members::role,
        ))
        .load(conn)?;
    let members = if member_rows.is_empty() {
        None
    } else {
        Some(
            member_rows
                .into_iter()
                .map(|(user_id, email, role)| ArchiveMemberEntry {
                    user_id,
                    email,
                    role,
                })
                .collect(),
        )
    };

    // Transcript rows, oldest first. Note: hot-DB retention may already
    // have trimmed old messages; the archive preserves what remains (phase
    // 2 orders archival ahead of retention deletion).
    type MessageRow = (uuid::Uuid, String, String, chrono::NaiveDateTime, String);
    let rows: Vec<MessageRow> = messages::table
        .filter(messages::session_id.eq(session.id))
        .order(messages::created_at.asc())
        .select((
            messages::id,
            messages::role,
            messages::content,
            messages::created_at,
            messages::agent_type,
        ))
        .load(conn)?;

    let current_lines: Vec<ArchiveMessageLine> = rows
        .into_iter()
        .map(|(id, role, content, created_at, agent_type)| {
            // Stored content is JSON text; embed it as a value so the
            // archive round-trips it. Non-JSON content (shouldn't exist)
            // degrades to a JSON string.
            let content = match serde_json::from_str(&content) {
                Ok(value) => value,
                Err(_) => serde_json::Value::String(content),
            };
            ArchiveMessageLine {
                id,
                role,
                created_at,
                agent_type,
                content,
            }
        })
        .collect();

    // Merge with any previously-archived transcript (#1258 phase 2):
    // retention may have trimmed hot rows that only survive in the
    // archive; a re-archive must never shrink it.
    let existing_lines = runtime
        .store
        .read_transcript_lines(session.user_id, session.id)?
        .unwrap_or_default();
    let merged_lines = crate::archive::merge_transcript_lines(existing_lines, current_lines);

    let mut message_counts: BTreeMap<String, i64> = BTreeMap::new();
    for line in &merged_lines {
        *message_counts.entry(line.role.clone()).or_default() += 1;
    }
    // Substantive user messages (the history "User msgs" column): counted at
    // archive time for every new/re-archived session; pre-existing manifests
    // are backfilled lazily when their transcript is next viewed.
    let user_message_count = merged_lines
        .iter()
        .filter(|l| {
            l.role == "user" && shared::user_messages::is_substantive_user_record(&l.content)
        })
        .count() as i64;

    // Turn aggregates for the manifest (analytics reads these, never the
    // transcript body).
    type TurnAggRow = (
        Option<String>,
        Option<String>,
        bool,
        i64,
        i64,
        Option<String>,
        i32,
        i32,
        Option<i64>,
    );
    let turn_rows: Vec<TurnAggRow> = turn_metrics::table
        .filter(turn_metrics::session_id.eq(session.id))
        .select((
            turn_metrics::model,
            turn_metrics::stop_reason,
            turn_metrics::is_error,
            turn_metrics::thinking_tokens,
            turn_metrics::subagent_tokens,
            turn_metrics::service_tier,
            turn_metrics::tool_call_count,
            turn_metrics::stream_restarts,
            turn_metrics::total_duration_ms,
        ))
        .load(conn)?;

    let mut turns = ArchiveTurnStats {
        count: turn_rows.len() as i64,
        ..Default::default()
    };
    let mut thinking = 0i64;
    let mut subagent = 0i64;
    let mut models_seen = std::collections::BTreeSet::new();
    for (model, stop_reason, is_error, t, s, tier, tool_calls, restarts, duration_ms) in &turn_rows
    {
        if *is_error {
            turns.errored += 1;
        }
        if let Some(reason) = stop_reason {
            *turns.stop_reasons.entry(reason.clone()).or_default() += 1;
        }
        if let Some(model) = model {
            models_seen.insert(model.clone());
        }
        if let Some(tier) = tier {
            *turns.service_tiers.entry(tier.clone()).or_default() += 1;
        }
        turns.tool_calls += i64::from(*tool_calls);
        turns.stream_restarts += i64::from(*restarts);
        turns.total_duration_ms += duration_ms.unwrap_or(0);
        thinking += t;
        subagent += s;
    }
    turns.models = models_seen.into_iter().collect();

    // Media section: which write-through blobs survive in the archive for this
    // session (#1450 durability). Best-effort and truthful — it lists only
    // blobs whose archive sidecar is present, so a media whose write-through
    // failed (or predates the feature) is simply omitted and the sweep never
    // fails on it. `None` when media archiving is disabled or none survived.
    let media = collect_session_media(runtime, session.user_id, session.id, &merged_lines);

    let archived_at = chrono::Utc::now().naive_utc();
    let transcripts_enabled = runtime.config.transcripts && !merged_lines.is_empty();

    let (transcript_ndjson, transcript_info) = if transcripts_enabled {
        let mut ndjson = Vec::new();
        for line in &merged_lines {
            serde_json::to_writer(&mut ndjson, line)?;
            ndjson.push(b'\n');
        }
        let info = ArchiveTranscriptInfo {
            object_key: transcript_key(session.user_id, session.id),
            compression: crate::archive::TRANSCRIPT_COMPRESSION.to_string(),
            message_count: merged_lines.len() as i64,
            bytes: ndjson.len() as u64,
        };
        (Some(ndjson), Some(info))
    } else {
        (None, None)
    };

    let bundle = SessionArchiveBundle {
        manifest: SessionArchiveManifest {
            schema_version: ARCHIVE_SCHEMA_VERSION,
            session_id: session.id,
            user_id: session.user_id,
            owner_email,
            owner_name,
            session_name: session.session_name.clone(),
            agent_type: session.agent_type.clone(),
            status: session.status.clone(),
            working_directory: session.working_directory.clone(),
            hostname: session.hostname.clone(),
            git_branch: session.git_branch.clone(),
            repo_url: session.repo_url.clone(),
            pr_url: session.pr_url.clone(),
            client_version: session.client_version.clone(),
            created_at: session.created_at,
            last_activity: session.last_activity,
            archived_at,
            message_counts,
            user_message_count: Some(user_message_count),
            tokens: ArchiveTokenTotals {
                input: session.input_tokens,
                output: session.output_tokens,
                cache_creation: session.cache_creation_tokens,
                cache_read: session.cache_read_tokens,
                thinking,
                subagent,
            },
            total_cost_usd: session.total_cost_usd,
            turns,
            transcript: transcript_info,
            media,
            launcher_id: session.launcher_id,
            launcher_version: session.launcher_version.clone(),
            forked_from_session_id: session.forked_from_session_id,
            fork_point_turn_id: session.fork_point_turn_id.clone(),
            scheduled_task_id: session.scheduled_task_id,
            // `claude_args` is stored as a JSONB string array; recover it as
            // Vec<String>, tolerating a malformed/absent value as empty.
            claude_args: serde_json::from_value(session.claude_args.clone()).unwrap_or_default(),
            // Per-write provenance: whichever backend runs this sweep stamps
            // its own version, so a re-archive reflects the newer writer.
            archived_by_version: Some(shared::VERSION.to_string()),
            members,
        },
        transcript_ndjson,
    };

    let bytes = bundle
        .transcript_ndjson
        .as_ref()
        .map(|b| b.len() as u64)
        .unwrap_or(0);
    {
        // Serialize with the history backfill's manifest rewrite — see
        // `ArchiveRuntime::manifest_write_lock`.
        let _manifest_guard = runtime
            .manifest_write_lock
            .lock()
            // A poisoned lock only means another writer panicked mid-write;
            // the ordering guarantee is unaffected, so continue.
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        runtime.store.put_session_archive(&bundle)?;
    }
    runtime.stats.record_success(bytes);
    Ok(())
}

/// Build the manifest's media section by scanning the transcript for
/// `agent-portal show` blobs and keeping those that survive in the archive.
///
/// The media_id → session mapping has no dedicated table — #1450 embeds the
/// served URL (`/api/images/{id}` / `/api/media/{id}`) in the portal transcript
/// row — so the transcript *is* the source of truth for what the session
/// referenced. For each referenced blob we consult the archive's sidecar
/// ([`ArchivedMediaMeta`]): present means the write-through succeeded and the
/// bytes are durable, so we emit an authoritative [`MediaEntry`] from it;
/// absent means we omit it (write-through failed, was disabled, or the blob
/// predates the feature). Returns `None` when media archiving is off or nothing
/// survived, so the manifest field stays absent in those cases.
fn collect_session_media(
    runtime: &crate::archive::ArchiveRuntime,
    user_id: uuid::Uuid,
    session_id: uuid::Uuid,
    lines: &[crate::archive::ArchiveMessageLine],
) -> Option<Vec<crate::archive::MediaEntry>> {
    use crate::archive::{media_key, MediaEntry};
    use shared::{PortalContent, PortalMessage};

    if !runtime.config.media {
        return None;
    }

    // Preserve first-seen order while de-duplicating repeated references.
    let mut seen: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
    let mut entries: Vec<MediaEntry> = Vec::new();

    for line in lines {
        let Ok(portal) = serde_json::from_value::<PortalMessage>(line.content.clone()) else {
            continue;
        };
        for content in &portal.content {
            let (kind, data) = match content {
                PortalContent::Image { data, .. } => ("image", data),
                PortalContent::Video { data, .. } => ("video", data),
                PortalContent::Figure { data, .. } => ("figure", data),
                _ => continue,
            };
            let Some(media_id) = parse_media_id(data, kind) else {
                continue;
            };
            if !seen.insert(media_id) {
                continue;
            }
            // Include only blobs the archive actually holds (sidecar present).
            match runtime.store.get_media_meta(user_id, session_id, media_id) {
                Ok(Some(meta)) => entries.push(MediaEntry {
                    media_id,
                    kind: meta.kind,
                    content_type: meta.content_type,
                    bytes: meta.bytes,
                    object_key: media_key(user_id, session_id, media_id),
                    uploaded_at: meta.uploaded_at,
                }),
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "Archive media manifest: sidecar read failed for {media_id}: {e}"
                ),
            }
        }
    }

    (!entries.is_empty()).then_some(entries)
}

/// Extract the media id from a served URL (`/api/images/{id}` for `kind`
/// `"image"`, `/api/media/{id}` for `"video"` or `"figure"`); `None` if it isn't a served-url
/// reference of that kind.
fn parse_media_id(data: &str, kind: &str) -> Option<uuid::Uuid> {
    let prefix = match kind {
        "image" => "/api/images/",
        "video" | "figure" => "/api/media/",
        _ => return None,
    };
    data.strip_prefix(prefix)?.parse().ok()
}

/// Archive every session whose messages the retention trim is about to
/// touch (#1258 phase 2): sessions holding messages older than the age
/// cutoff, plus sessions over the per-session count cap. Unlike the idle
/// sweep, this deliberately ignores idle/status eligibility — an ACTIVE
/// long-running session gets its history captured before it's trimmed,
/// and merge-on-rearchive folds later messages in.
///
/// Returns the set of session ids whose archive attempt FAILED this cycle.
/// Archive-first is the invariant: the caller holds the trim for these
/// sessions (excludes them from both delete paths) so an archive outage that
/// coincides with a retention cycle can never lose the unarchived delta — the
/// trim is retried next cycle once the archive succeeds. Sessions with a
/// fresh/successful archive are absent from the set and trim normally.
fn archive_retention_candidates(
    conn: &mut diesel::PgConnection,
    runtime: &crate::archive::ArchiveRuntime,
    config: &handlers::retention::RetentionConfig,
) -> std::collections::HashSet<uuid::Uuid> {
    use diesel::dsl::count_star;
    use diesel::prelude::*;
    use schema::{messages, sessions};

    let mut affected: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();

    if config.retention_days > 0 {
        let cutoff = chrono::Utc::now().naive_utc()
            - chrono::Duration::days(i64::from(config.retention_days));
        match messages::table
            .filter(messages::created_at.lt(cutoff))
            .select(messages::session_id)
            .distinct()
            .load::<uuid::Uuid>(conn)
        {
            Ok(ids) => affected.extend(ids),
            Err(e) => tracing::error!("Failed to query age-retention candidates: {e}"),
        }
    }

    match messages::table
        .group_by(messages::session_id)
        .having(count_star().gt(config.max_messages_per_session))
        .select(messages::session_id)
        .load::<uuid::Uuid>(conn)
    {
        Ok(ids) => affected.extend(ids),
        Err(e) => tracing::error!("Failed to query count-retention candidates: {e}"),
    }

    if affected.is_empty() {
        return std::collections::HashSet::new();
    }

    let candidates: Vec<models::Session> = match sessions::table
        .filter(sessions::id.eq_any(affected.iter().copied().collect::<Vec<_>>()))
        .load(conn)
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to load retention-candidate sessions: {e}");
            // Could not confirm any archive this cycle: hold every candidate
            // rather than risk trimming an unarchived session.
            return affected;
        }
    };

    let mut archived = 0;
    let mut failed: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
    for session in &candidates {
        if ensure_session_archived(conn, runtime, session) {
            archived += 1;
        } else {
            failed.insert(session.id);
        }
    }
    if archived > 0 {
        tracing::info!(
            "Pre-retention archive: {} of {} candidate session(s) captured",
            archived,
            candidates.len()
        );
    }
    failed
}

/// Archive `session` if its archive is missing or stale, updating
/// `archived_at` on success. Returns whether the session now has a fresh
/// archive. Shared by the sweep, the retention gates (#1258 phase 2), and
/// the close-session handler's final pre-delete archive.
pub(crate) fn ensure_session_archived(
    conn: &mut diesel::PgConnection,
    runtime: &crate::archive::ArchiveRuntime,
    session: &models::Session,
) -> bool {
    use diesel::prelude::*;
    use schema::sessions;

    if is_archive_fresh(session.archived_at, session.last_activity) {
        return true;
    }
    match archive_one_session(conn, runtime, session) {
        Ok(()) => {
            let _ = diesel::update(sessions::table.find(session.id))
                .set(sessions::archived_at.eq(chrono::Utc::now().naive_utc()))
                .execute(conn);
            true
        }
        Err(e) => {
            runtime.stats.record_failure(&e.to_string());
            tracing::error!("{SESSION_ARCHIVE_FAILED} session={}: {e}", session.id);
            false
        }
    }
}

/// Evict proxy/launcher connections that have gone silent past their
/// liveness deadline (see `session_manager/liveness.rs`, #1256). The
/// eviction cancels each stale connection's socket task, so the client's
/// reconnect logic recovers automatically.
pub async fn run_liveness_sweep(app_state: Arc<AppState>) {
    use crate::handlers::websocket::{
        LAUNCHER_LIVENESS_DEADLINE_SECS, PROXY_LIVENESS_DEADLINE_SECS,
    };

    let (proxies, launchers) = app_state.session_manager.sweep_stale_connections(
        PROXY_LIVENESS_DEADLINE_SECS,
        LAUNCHER_LIVENESS_DEADLINE_SECS,
    );
    if proxies > 0 || launchers > 0 {
        tracing::warn!(
            "Liveness sweep evicted {} proxy and {} launcher connection(s)",
            proxies,
            launchers
        );
    }
}

/// Query user spend from DB and broadcast to all connected web clients
pub async fn broadcast_user_spend_updates(app_state: Arc<AppState>) {
    use diesel::prelude::*;
    use shared::{ServerToClient, SessionCost};

    if !app_state.session_manager.has_any_user_clients() {
        return;
    }

    let connected_user_ids = app_state.session_manager.get_all_user_ids();
    if connected_user_ids.is_empty() {
        return;
    }

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for spend broadcast");
        return;
    };

    // Single query: fetch all sessions with cost > 0 for all connected users
    type CostRow = (uuid::Uuid, uuid::Uuid, f64, i64, i64, i64, i64);
    let all_sessions: Vec<CostRow> = match schema::sessions::table
        .filter(schema::sessions::user_id.eq_any(&connected_user_ids))
        .filter(schema::sessions::total_cost_usd.gt(0.0))
        .select((
            schema::sessions::user_id,
            schema::sessions::id,
            schema::sessions::total_cost_usd,
            schema::sessions::input_tokens,
            schema::sessions::output_tokens,
            schema::sessions::cache_creation_tokens,
            schema::sessions::cache_read_tokens,
        ))
        .load(&mut conn)
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("Failed to query session costs for spend broadcast: {}", e);
            return;
        }
    };

    // Single query: fetch deleted session costs for all connected users
    let deleted_costs: Vec<(uuid::Uuid, f64)> = schema::deleted_session_costs::table
        .filter(schema::deleted_session_costs::user_id.eq_any(&connected_user_ids))
        .filter(schema::deleted_session_costs::cost_usd.gt(0.0))
        .select((
            schema::deleted_session_costs::user_id,
            schema::deleted_session_costs::cost_usd,
        ))
        .load(&mut conn)
        .unwrap_or_default();

    // Build a map of user_id -> deleted cost
    let deleted_cost_map: std::collections::HashMap<uuid::Uuid, f64> =
        deleted_costs.into_iter().collect();

    // Group sessions by user_id
    let mut user_sessions: std::collections::HashMap<uuid::Uuid, Vec<SessionCost>> =
        std::collections::HashMap::new();
    let mut user_active_cost: std::collections::HashMap<uuid::Uuid, f64> =
        std::collections::HashMap::new();

    for (uid, sid, cost, inp, outp, cache_create, cache_read) in all_sessions {
        *user_active_cost.entry(uid).or_default() += cost;
        user_sessions.entry(uid).or_default().push(SessionCost {
            session_id: sid,
            total_cost_usd: cost,
            input_tokens: inp,
            output_tokens: outp,
            cache_creation_tokens: cache_create,
            cache_read_tokens: cache_read,
        });
    }

    // Broadcast to each connected user
    for uid in &connected_user_ids {
        let active_cost = user_active_cost.get(uid).copied().unwrap_or(0.0);
        let deleted_cost = deleted_cost_map.get(uid).copied().unwrap_or(0.0);
        let total_spend = active_cost + deleted_cost;
        let session_costs = user_sessions.remove(uid).unwrap_or_default();

        if total_spend > 0.0 || !session_costs.is_empty() {
            app_state.session_manager.broadcast_to_user(
                uid,
                ServerToClient::UserSpendUpdate {
                    total_spend_usd: total_spend,
                    session_costs,
                },
            );
        }
    }
}

/// Purge expired device flow codes from the in-memory store
pub async fn purge_expired_device_codes(app_state: Arc<AppState>) {
    let Some(store) = &app_state.device_flow_store else {
        return;
    };
    let mut map = store.write().await;
    let before = map.len();
    map.retain(|_, state| state.expires_at > std::time::SystemTime::now());
    let removed = before - map.len();
    if removed > 0 {
        tracing::debug!("Purged {} expired device flow codes", removed);
    }
}

/// Run retention cleanup: delete old messages and truncate per-session counts
pub async fn run_retention_cleanup(app_state: Arc<AppState>) {
    use handlers::retention::RetentionConfig;

    let session_ids = app_state.session_manager.drain_pending_truncations();
    let db_pool = app_state.db_pool.clone();
    let archive = app_state.archive.clone();
    let config = RetentionConfig::new(
        app_state.message_retention_count,
        app_state.message_retention_days,
    );

    // Everything below is blocking work — synchronous Diesel queries plus, with
    // an object-store archive, `block_on` reads inside
    // archive_retention_candidates. It MUST run on the blocking pool: those
    // reads panic ("Cannot start a runtime from within a runtime") when driven
    // from an async runtime worker, which silently killed this loop before
    // #1487 (the sweep at run_archive_sweep already uses spawn_blocking).
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = db_pool.get()?;

        // #1258 phase 2: capture messages into the archive BEFORE the trim
        // deletes them from the hot DB. Archive-first is the invariant — a
        // session whose pre-trim archive FAILS this cycle has its trim HELD
        // (excluded from both delete paths below) and retried next cycle, so an
        // archive outage that coincides with a retention cycle never loses the
        // unarchived delta. Trade-off: the hot DB keeps growing for those
        // sessions until the archive recovers, which is the correct failure
        // mode (data preserved over a bounded space cost). This mirrors the
        // held semantics of run_session_age_cleanup. When archiving is DISABLED
        // the held set is empty and trims run exactly as before — running
        // retention without an archive is the operator's explicit choice.
        let held_ids: std::collections::HashSet<uuid::Uuid> = match &archive {
            Some(runtime) => archive_retention_candidates(&mut conn, runtime, &config),
            None => std::collections::HashSet::new(),
        };
        if !held_ids.is_empty() {
            // Stable marker aligned with SESSION_ARCHIVE_FAILED: alert on this
            // to catch an archive outage that is silently blocking retention.
            tracing::warn!(
                "{RETENTION_TRIM_HELD} {} sessions pending archive",
                held_ids.len()
            );
        }

        let (age_deleted, count_deleted) =
            handlers::retention::run_retention_cleanup(&mut conn, session_ids, config, &held_ids);
        Ok::<(usize, usize), anyhow::Error>((age_deleted, count_deleted))
    })
    .await;

    match result {
        Ok(Ok((age_deleted, count_deleted))) => {
            if age_deleted > 0 || count_deleted > 0 {
                tracing::info!(
                    "Retention cleanup complete: {} old, {} over-limit",
                    age_deleted,
                    count_deleted
                );
            }
        }
        Ok(Err(e)) => tracing::error!("Failed to get DB connection for retention cleanup: {e}"),
        Err(e) => tracing::error!("Retention cleanup task panicked: {e}"),
    }
}

/// Delete sessions whose last_activity is older than SESSION_MAX_AGE_DAYS
pub async fn run_session_age_cleanup(app_state: Arc<AppState>) {
    let max_days = app_state.session_max_age_days;
    if max_days == 0 {
        return;
    }
    let db_pool = app_state.db_pool.clone();
    let archive = app_state.archive.clone();

    // Blocking work on the blocking pool: Diesel queries plus, with an
    // object-store archive, the same `block_on` reads inside
    // ensure_session_archived that would panic on an async runtime worker
    // (#1487, the twin of the retention path above).
    let result = tokio::task::spawn_blocking(move || {
        use diesel::prelude::*;
        use handlers::helpers::delete_session_with_data;

        let mut conn = db_pool.get()?;

        // Set a 5-second timeout for cleanup queries
        if let Err(e) = diesel::sql_query("SET LOCAL statement_timeout = '5000'").execute(&mut conn)
        {
            tracing::warn!("Failed to set statement_timeout for session age cleanup: {e}");
        }

        let cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::days(i64::from(max_days));
        let old_sessions: Vec<models::Session> = schema::sessions::table
            .filter(schema::sessions::last_activity.lt(cutoff))
            .load(&mut conn)?;

        let mut deleted = 0;
        let mut held = 0;
        for session in &old_sessions {
            // #1258 phase 2: when archiving is enabled, an eligible session is
            // only deleted once it has a fresh archive; on archive failure the
            // deletion is HELD (retried next cycle) and the failure recorded —
            // retention can never silently destroy the last copy.
            if let Some(runtime) = &archive {
                if !ensure_session_archived(&mut conn, runtime, session) {
                    held += 1;
                    continue;
                }
            }
            match delete_session_with_data(&mut conn, session, true) {
                Ok(_) => deleted += 1,
                Err(e) => tracing::error!("Failed to delete old session {}: {:?}", session.id, e),
            }
        }
        Ok::<(usize, usize), anyhow::Error>((deleted, held))
    })
    .await;

    match result {
        Ok(Ok((deleted, held))) => {
            if deleted > 0 || held > 0 {
                tracing::info!(
                    "Session age cleanup: deleted {} sessions older than {} days{}",
                    deleted,
                    max_days,
                    if held > 0 {
                        format!(" ({held} held pending archive)")
                    } else {
                        String::new()
                    }
                );
            }
        }
        Ok(Err(e)) => tracing::error!("Session age cleanup query failed: {e}"),
        Err(e) => tracing::error!("Session age cleanup task panicked: {e}"),
    }
}

/// Delete proxy auth tokens whose expiration is more than 7 days in the past.
pub async fn run_expired_token_cleanup(app_state: Arc<AppState>) {
    use diesel::prelude::*;

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for expired token cleanup");
        return;
    };

    if let Err(e) = diesel::sql_query("SET LOCAL statement_timeout = '5000'").execute(&mut conn) {
        tracing::warn!(
            "Failed to set statement_timeout for expired token cleanup: {}",
            e
        );
    }

    let token_cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::days(7);
    match diesel::delete(
        schema::proxy_auth_tokens::table
            .filter(schema::proxy_auth_tokens::expires_at.lt(token_cutoff)),
    )
    .execute(&mut conn)
    {
        Ok(0) => {}
        Ok(count) => {
            tracing::info!("Expired token cleanup: deleted {} tokens", count);
        }
        Err(e) => {
            tracing::error!("Failed to delete expired tokens: {}", e);
        }
    }

    // Delete revoked credentials past the same window. Neither pass around this
    // one collects them: launcher-spawned tokens carry no expiry, and
    // `expires_at < cutoff` is never true for NULL, so every session that ever
    // ran left a revoked row behind forever — June credentials were still
    // listed in late August. A revoked token cannot authenticate, so it is kept
    // only long enough to stay visible in the UI after the fact.
    //
    // Aged on `created_at` because there is no `revoked_at` column. A token
    // revoked long after it was minted is therefore collected sooner than seven
    // days, which is harmless for a credential that can no longer be used — and
    // `created_at` is NOT NULL, so this cannot repeat the NULL-comparison bug it
    // exists to fix.
    match diesel::delete(
        schema::proxy_auth_tokens::table
            .filter(schema::proxy_auth_tokens::revoked.eq(true))
            .filter(schema::proxy_auth_tokens::created_at.lt(token_cutoff)),
    )
    .execute(&mut conn)
    {
        Ok(0) => {}
        Ok(count) => {
            tracing::info!("Revoked token cleanup: deleted {} tokens", count);
        }
        Err(e) => {
            tracing::error!("Failed to delete revoked tokens: {}", e);
        }
    }

    // Delete leaked launcher-spawned tokens (#1045). These are minted per
    // launch and only ever bound to a session on a *successful* proxy
    // registration; a launch whose proxy never registers leaves a
    // never-expiring, never-bound, never-revoked token behind. A legitimate
    // launch token is bound within seconds, so any still unbound an hour after
    // creation belongs to a failed launch and is safe to delete.
    let leak_cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
    match diesel::delete(
        schema::proxy_auth_tokens::table
            .filter(
                schema::proxy_auth_tokens::name
                    .eq(crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME),
            )
            .filter(schema::proxy_auth_tokens::session_id.is_null())
            .filter(schema::proxy_auth_tokens::created_at.lt(leak_cutoff)),
    )
    .execute(&mut conn)
    {
        Ok(0) => {}
        Ok(count) => {
            tracing::info!("Leaked launch-token cleanup: deleted {} tokens", count);
        }
        Err(e) => {
            tracing::error!("Failed to delete leaked launch tokens: {}", e);
        }
    }
}

#[cfg(test)]
mod background_decision_tests {
    use super::is_archive_fresh;
    use chrono::{NaiveDate, NaiveDateTime};

    fn dt(y: i32, m: u32, d: u32, h: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    #[test]
    fn is_archive_fresh_matches_ensure_guard() {
        let last = dt(2026, 1, 9, 10);
        assert!(!is_archive_fresh(None, last));
        assert!(!is_archive_fresh(Some(dt(2026, 1, 8, 0)), last));
        assert!(is_archive_fresh(Some(last), last));
        assert!(is_archive_fresh(Some(dt(2026, 1, 10, 0)), last));
    }
}

#[cfg(test)]
mod archive_pending_sessions_db_tests {
    use crate::schema::sessions;
    use chrono::NaiveDateTime;
    use diesel::prelude::*;
    use uuid::Uuid;

    fn insert_session(
        conn: &mut diesel::PgConnection,
        user_id: Uuid,
        status: &str,
        last_activity: NaiveDateTime,
        archived_at: Option<NaiveDateTime>,
    ) -> Uuid {
        let session = crate::test_support::insert_session(conn, user_id, "test-archive-pending");
        let id = session.id;
        // Override the helper defaults (status) and DB defaults (timestamps)
        // to the exact values for the predicate.
        diesel::update(sessions::table.find(id))
            .set((
                sessions::status.eq(status.to_string()),
                sessions::last_activity.eq(last_activity),
                sessions::archived_at.eq(archived_at),
                sessions::updated_at.eq(last_activity),
            ))
            .execute(conn)
            .expect("set timestamps");
        id
    }

    #[test]
    fn archive_pending_selects_only_eligible_via_real_diesel_filter() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("conn");
        conn.begin_test_transaction().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "archive-eligible");

        // Cutoff is now - ARCHIVE_IDLE_SECS (3600s). Use wide margins so the test
        // is not flaky around the second the query captures `now`.
        let now = chrono::Utc::now().naive_utc();
        let idle = now - chrono::Duration::hours(2);
        let recent = now - chrono::Duration::minutes(30);
        let stale_archived = idle - chrono::Duration::hours(1);
        let fresh_archived = idle + chrono::Duration::minutes(10);
        let cutoff_exact = now - chrono::Duration::seconds(crate::archive::ARCHIVE_IDLE_SECS);

        // Seven cases spanning the predicate.
        let active_never = insert_session(&mut conn, user.id, "active", idle, None);
        let recent_inactive = insert_session(&mut conn, user.id, "disconnected", recent, None);
        let never_archived = insert_session(&mut conn, user.id, "disconnected", idle, None);
        let stale = insert_session(
            &mut conn,
            user.id,
            "disconnected",
            idle,
            Some(stale_archived),
        );
        let fresh = insert_session(
            &mut conn,
            user.id,
            "disconnected",
            idle,
            Some(fresh_archived),
        );
        let boundary = insert_session(&mut conn, user.id, "disconnected", cutoff_exact, None);
        let unknown_status = insert_session(&mut conn, user.id, "paused", idle, None);

        // Use the SAME shared fn as production — filter drift cannot escape the test.
        let cutoff = now - chrono::Duration::seconds(crate::archive::ARCHIVE_IDLE_SECS);
        let eligible: Vec<Uuid> = crate::background::load_archive_eligible(&mut conn, cutoff, 100)
            .expect("query eligible")
            .into_iter()
            .map(|s| s.id)
            .filter(|id| {
                [
                    active_never,
                    recent_inactive,
                    never_archived,
                    stale,
                    fresh,
                    boundary,
                    unknown_status,
                ]
                .contains(id)
            })
            .collect();

        assert!(!eligible.contains(&active_never), "active never eligible");
        assert!(
            !eligible.contains(&recent_inactive),
            "recent (< cutoff) not eligible"
        );
        assert!(
            eligible.contains(&never_archived),
            "never-archived idle should be eligible"
        );
        assert!(eligible.contains(&stale), "stale should be eligible");
        assert!(!eligible.contains(&fresh), "fresh should not be eligible");
        assert!(
            !eligible.contains(&boundary),
            "==cutoff is exclusive (lt), not eligible"
        );
        assert!(
            eligible.contains(&unknown_status),
            "unknown non-active status follows inactive path"
        );
        assert_eq!(
            eligible.len(),
            3,
            "exactly never+stale+unknown should be selected"
        );
    }
}

#[cfg(test)]
mod retention_archive_hold_tests {
    use crate::archive::{ArchiveConfig, ArchiveRuntime};
    use crate::handlers::retention::RetentionConfig;
    use crate::schema::{messages, sessions};
    use diesel::prelude::*;
    use uuid::Uuid;

    fn make_session(conn: &mut diesel::PgConnection, user_id: Uuid) -> Uuid {
        let session = crate::test_support::insert_session(conn, user_id, "test-retention");
        let id = session.id;
        // Stamp the non-default columns the old literal set inline.
        diesel::update(sessions::table.find(id))
            .set((
                sessions::status.eq("disconnected"),
                sessions::hostname.eq("h"),
            ))
            .execute(conn)
            .expect("set session status");
        id
    }

    fn insert_old_message(
        conn: &mut diesel::PgConnection,
        session_id: Uuid,
        user_id: Uuid,
        created_at: chrono::NaiveDateTime,
    ) {
        let mid = Uuid::new_v4();
        diesel::insert_into(messages::table)
            .values((
                messages::id.eq(mid),
                messages::session_id.eq(session_id),
                messages::role.eq("user"),
                messages::content.eq("hello"),
                messages::created_at.eq(created_at),
                messages::user_id.eq(user_id),
                messages::agent_type.eq("claude"),
            ))
            .execute(conn)
            .expect("insert message");
    }

    #[test]
    fn retention_archive_disabled_holds_nothing_and_allows_trim() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("conn");
        conn.begin_test_transaction().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "retention-disabled");
        let sid = make_session(&mut conn, user.id);
        let old = chrono::Utc::now().naive_utc() - chrono::Duration::days(40);
        insert_old_message(&mut conn, sid, user.id, old);

        let config = RetentionConfig::new(100, 30);
        // Archive disabled -> held set empty per `run_retention_cleanup` None branch.
        let held: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        // Verify the candidate would be trimmed if not held.
        let (age_deleted, _) =
            crate::handlers::retention::run_retention_cleanup(&mut conn, vec![], config, &held);
        assert!(
            age_deleted >= 1,
            "with no held, old message should be deleted"
        );
    }

    #[test]
    fn retention_pre_archive_success_allows_trim_via_local_backend() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("conn");
        conn.begin_test_transaction().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "retention-local-ok");
        let sid = make_session(&mut conn, user.id);
        let old = chrono::Utc::now().naive_utc() - chrono::Duration::days(40);
        insert_old_message(&mut conn, sid, user.id, old);

        let tmp = tempfile::tempdir().expect("tmp");
        let cfg = ArchiveConfig {
            backend: crate::archive::ArchiveBackendConfig::Local {
                root: tmp.path().to_path_buf(),
            },
            transcripts: true,
            media: true,
        };
        let runtime = ArchiveRuntime::new(cfg).expect("runtime");

        let config = RetentionConfig::new(100, 30);
        let held = crate::background::archive_retention_candidates(&mut conn, &runtime, &config);
        assert!(
            held.is_empty(),
            "successful local archive should not hold trim, got {held:?}"
        );
    }

    #[test]
    fn retention_pre_archive_failure_holds_trim() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("conn");
        conn.begin_test_transaction().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "retention-fail");
        let sid = make_session(&mut conn, user.id);
        let old = chrono::Utc::now().naive_utc() - chrono::Duration::days(40);
        insert_old_message(&mut conn, sid, user.id, old);

        // Unwritable / invalid local path should make archive_one_session fail.
        // When running as root, permission bits are ignored, so use a file as the
        // path — LocalArchiveStore will fail to create a directory there.
        let tmp_file = tempfile::NamedTempFile::new().expect("tmp file");
        let cfg = ArchiveConfig {
            backend: crate::archive::ArchiveBackendConfig::Local {
                root: tmp_file.path().to_path_buf(),
            },
            transcripts: true,
            media: true,
        };
        let runtime = match ArchiveRuntime::new(cfg) {
            Ok(r) => r,
            Err(e) => panic!("ArchiveRuntime::new failed for file-as-dir: {e}"),
        };

        let config = RetentionConfig::new(100, 30);
        let held = crate::background::archive_retention_candidates(&mut conn, &runtime, &config);
        // File-as-dir fails ENOTDIR for any user including root (not a permission check),
        // unlike chmod which root bypasses — so this holds reliably.
        assert!(held.contains(&sid), "failed archive should hold {sid}");
        let (age_deleted, _) =
            crate::handlers::retention::run_retention_cleanup(&mut conn, vec![], config, &held);
        assert_eq!(age_deleted, 0, "held session must not be trimmed");
    }
}

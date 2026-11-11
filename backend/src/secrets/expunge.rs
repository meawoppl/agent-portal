//! Durable, restartable cleanup. Encrypted vault versions are never deleted.
//! Every receipt reports backup/opaque-media limits rather than claiming that a
//! successful SQL UPDATE erased WAL, object history or downloaded copies.
use super::{
    redaction::{self, Redactor},
    store,
};
use crate::schema::secret_expunge_jobs as jobs;
use crate::{errors::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use shared::api::{SecretExpungeJob, SecretExpungeRequest, SecretExpungeState};
use std::{
    io::{BufRead, Seek, Write},
    sync::Arc,
};
use tower_cookies::Cookies;
use uuid::Uuid;

const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
#[derive(Queryable, Selectable)]
#[diesel(table_name = jobs)]
struct JobRow {
    id: Uuid,
    secret_id: Uuid,
    requested_by: Uuid,
    state: String,
    checkpoint: serde_json::Value,
    receipt: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct CleanupScope {
    global: bool,
    approved_versions: Vec<Uuid>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Receipt {
    records_redacted: i64,
    objects_rewritten: i64,
    bytes_scanned: i64,
    blocked_targets: i64,
    limitations: Vec<String>,
}
impl JobRow {
    fn api(&self) -> SecretExpungeJob {
        let receipt = serde_json::from_value::<Receipt>(self.receipt.clone()).unwrap_or_default();
        let state = match self.state.as_str() {
            "queued" => SecretExpungeState::Queued,
            "discovering" => SecretExpungeState::Discovering,
            "redacting" => SecretExpungeState::Redacting,
            "deleting_versions" => SecretExpungeState::DeletingVersions,
            "verifying" => SecretExpungeState::Verifying,
            "complete_active_storage" => SecretExpungeState::CompleteActiveStorage,
            "awaiting_backup_expiry" => SecretExpungeState::AwaitingBackupExpiry,
            "blocked" => SecretExpungeState::Blocked,
            _ => SecretExpungeState::Failed,
        };
        SecretExpungeJob {
            id: self.id,
            secret_id: self.secret_id,
            state,
            records_redacted: receipt.records_redacted,
            objects_rewritten: receipt.objects_rewritten,
            bytes_scanned: receipt.bytes_scanned,
            blocked_targets: receipt.blocked_targets,
            limitations: receipt.limitations,
            created_at: self.created_at.to_rfc3339(),
            updated_at: self.updated_at.to_rfc3339(),
        }
    }
}

pub async fn enqueue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Path(id): Path<Uuid>,
    Json(request): Json<SecretExpungeRequest>,
) -> Result<Json<SecretExpungeJob>, AppError> {
    let principal = crate::handlers::secrets::principal(&state, &headers, &cookies, true)?;
    if !request.all_versions {
        return Err(AppError::BadRequest(
            "Expungement must include all retained versions",
        ));
    }
    state.secrets.refresh_redaction(&state.db_pool).await?;
    let mut conn = state.conn()?;
    let secret = store::require_manage(&mut conn, id, principal.user)?;
    if request.global && !store::curator(&mut conn, principal.user)? {
        return Err(AppError::Forbidden);
    }
    let job_id = if let Some(key) = headers.get("idempotency-key").and_then(|s| s.to_str().ok()) {
        if key.len() > 128 {
            return Err(AppError::BadRequest("Idempotency key exceeds limit"));
        }
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"portal-secret-expunge-v1");
        digest.update(principal.user.as_bytes());
        digest.update(id.as_bytes());
        digest.update(key.as_bytes());
        let hash = digest.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&hash[..16]);
        Uuid::from_bytes(bytes)
    } else {
        Uuid::new_v4()
    };
    let row=conn.transaction::<_,AppError,_>(|conn| {
        if let Some(row)=jobs::table.find(job_id).select(JobRow::as_select()).first::<JobRow>(conn).optional()? {
            if serde_json::from_value::<CleanupScope>(row.checkpoint.clone()).unwrap_or_default().global != request.global {
                return Err(AppError::Conflict("Idempotency key was already used with another cleanup scope"));
            }
            return Ok(row);
        }
        store::require_manage(conn,id,principal.user)?;
        let mut approved_versions=Vec::new();
        if request.global {
            use crate::schema::{secret_occurrences as occurrences,secret_versions,secrets_generation};
            if !store::curator(conn,principal.user)? {return Err(AppError::Forbidden)}
            diesel::sql_query("SELECT generation FROM secrets_generation WHERE id=1 FOR UPDATE").execute(conn)?;
            let versions=secret_versions::table.filter(secret_versions::secret_id.eq(id)).select(secret_versions::id).load::<Uuid>(conn)?;
            approved_versions=versions.clone();
            let mut added=0;
            for version in versions {
                added+=diesel::insert_into(occurrences::table).values((occurrences::rule_version_id.eq(version),occurrences::record_kind.eq("global_redaction"),occurrences::record_id.eq("global")))
                    .on_conflict_do_nothing().execute(conn)?;
            }
            if added>0 {diesel::update(secrets_generation::table.find(1i16)).set(secrets_generation::generation.eq(secrets_generation::generation+1)).execute(conn)?;}
        }
        let generation=crate::schema::secrets_generation::table.find(1i16).select(crate::schema::secrets_generation::generation).first::<i64>(conn)?;
        let receipt=Receipt { limitations:vec![
            "Encrypted vault history is retained; expunge does not delete secret versions.".into(),
            "Database WAL, dead tuples, backups and external/downloaded copies require separate retention or remediation.".into(),
            if request.global { "Organization-wide cleanup was explicitly authorized by a curator.".into() } else { "Cleanup is scoped to your own sessions and archives; cross-user cleanup requires an organization curator.".into() },
        ],..Default::default() };
        diesel::insert_into(jobs::table).values((jobs::id.eq(job_id),jobs::organization_id.eq(secret.organization_id),jobs::secret_id.eq(id),jobs::requested_by.eq(principal.user),jobs::generation.eq(generation),jobs::checkpoint.eq(serde_json::to_value(CleanupScope { global: request.global, approved_versions }).map_err(|_|AppError::Internal("Cannot record cleanup scope".into()))?),jobs::receipt.eq(serde_json::to_value(receipt).map_err(|_|AppError::Internal("Cannot record cleanup receipt".into()))?)))
            .on_conflict(jobs::id).do_nothing().execute(conn)?;
        store::audit(conn,principal.user,Some(id),"expunge_requested","allowed")?;
        Ok(jobs::table.find(job_id).select(JobRow::as_select()).first::<JobRow>(conn)?)
    })?;
    drop(conn);
    state.secrets.refresh_redaction(&state.db_pool).await?;
    Ok(Json(row.api()))
}

pub async fn get_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretExpungeJob>, AppError> {
    let principal = crate::handlers::secrets::principal(&state, &headers, &cookies, false)?;
    if !principal.metadata {
        return Err(AppError::Forbidden);
    }
    let mut conn = state.conn()?;
    let row = jobs::table
        .find(id)
        .select(JobRow::as_select())
        .first::<JobRow>(&mut conn)
        .map_err(|_| AppError::NotFound("Cleanup job not found"))?;
    store::require_manage(&mut conn, row.secret_id, principal.user)?;
    Ok(Json(row.api()))
}

pub fn spawn_worker(state: Arc<AppState>) {
    if let Some(runtime) = &state.archive {
        if let Ok(mut pool) = runtime.media_db_pool.write() {
            *pool = Some(state.db_pool.clone());
        }
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            if state
                .secrets
                .refresh_redaction(&state.db_pool)
                .await
                .is_err()
            {
                continue;
            }
            let worker = state.clone();
            match tokio::task::spawn_blocking(move || run_next(&worker)).await {
                Ok(Ok(())) => {}
                _ => tracing::error!(
                    "{}: cleanup worker will retry",
                    crate::markers::SECRET_EXPUNGE_FAILED
                ),
            }
        }
    });
}
fn save(conn: &mut PgConnection, id: Uuid, state: &str, receipt: &Receipt) -> Result<(), AppError> {
    diesel::update(jobs::table.find(id))
        .set((
            jobs::state.eq(state),
            jobs::receipt.eq(serde_json::to_value(receipt)
                .map_err(|_| AppError::Internal("Cannot record cleanup receipt".into()))?),
            jobs::updated_at.eq(Utc::now()),
        ))
        .execute(conn)?;
    Ok(())
}
fn run_next(state: &AppState) -> Result<(), AppError> {
    let mut conn = state.conn()?;
    if let Some(runtime) = &state.archive {
        use crate::schema::secret_coverage as coverage;
        let tombstones = coverage::table
            .filter(coverage::object_id.like("deleted-media/%"))
            .select(coverage::object_id)
            .limit(4096)
            .load::<String>(&mut conn)?;
        let mut suppressed = runtime
            .suppressed_media
            .write()
            .map_err(|_| AppError::ServiceUnavailable("Media cleanup state unavailable"))?;
        for key in tombstones {
            if let Some(id) = key
                .rsplit('/')
                .next()
                .and_then(|id| Uuid::parse_str(id).ok())
            {
                if suppressed.len() < 4096 {
                    suppressed.insert(id);
                }
            }
        }
    }
    let row = jobs::table
        .filter(
            jobs::state.eq("queued").or(jobs::state
                .eq_any(["discovering", "redacting", "deleting_versions", "verifying"])
                .and(jobs::updated_at.lt(Utc::now() - chrono::Duration::minutes(10)))),
        )
        .order(jobs::created_at.asc())
        .select(JobRow::as_select())
        .first::<JobRow>(&mut conn)
        .optional()?;
    let Some(job) = row else { return Ok(()) };
    // A dedicated connection is used for the advisory session lease, never
    // returned to the shared pool with an unreleased lock after panic.
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| AppError::ServiceUnavailable("Database configuration missing"))?;
    let mut lease = PgConnection::establish(&url).map_err(|_| AppError::DbPool)?;
    #[derive(QueryableByName)]
    struct Lock {
        #[diesel(sql_type=diesel::sql_types::Bool)]
        acquired: bool,
    }
    let lock =
        diesel::sql_query("SELECT pg_try_advisory_lock(hashtextextended($1, 0)) AS acquired")
            .bind::<diesel::sql_types::Text, _>(job.id.to_string())
            .get_result::<Lock>(&mut lease)?;
    if !lock.acquired {
        return Ok(());
    }
    let scope = serde_json::from_value::<CleanupScope>(job.checkpoint.clone()).unwrap_or_default();
    let mut receipt = serde_json::from_value::<Receipt>(job.receipt.clone()).unwrap_or_default();
    let authorized = store::require_manage(&mut conn, job.secret_id, job.requested_by).is_ok();
    let global = scope.global && store::curator(&mut conn, job.requested_by).unwrap_or(false);
    if !authorized || (scope.global && !global) {
        receipt.blocked_targets += 1;
        receipt.limitations.push(
            "Cleanup authorization is no longer valid; request a newly authorized job.".into(),
        );
        return save(&mut conn, job.id, "blocked", &receipt);
    }
    drop(conn);
    let rules =
        tokio::runtime::Handle::current().block_on(state.secrets.redactor_for_secret_versions(
            &state.db_pool,
            job.secret_id,
            if global {
                Some(scope.approved_versions.as_slice())
            } else {
                None
            },
        ))?;
    let mut conn = state.conn()?;
    if global {
        use crate::schema::secret_versions;
        let actual = secret_versions::table
            .filter(secret_versions::secret_id.eq(job.secret_id))
            .select(secret_versions::id)
            .load::<Uuid>(&mut conn)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let approved = scope
            .approved_versions
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if approved.is_empty() || actual != approved {
            receipt.blocked_targets += 1;
            receipt.limitations.push("The key gained versions after organization-wide approval; a curator must approve a new cleanup job.".into());
            return save(&mut conn, job.id, "blocked", &receipt);
        }
    }
    save(&mut conn, job.id, "discovering", &receipt)?;
    let result = cleanup(
        state,
        &mut conn,
        &rules,
        &mut receipt,
        job.id,
        job.secret_id,
        if global { None } else { Some(job.requested_by) },
    );
    let final_state = match result {
        Ok(()) if receipt.blocked_targets == 0 => "awaiting_backup_expiry",
        Ok(()) => "blocked",
        Err(_) => {
            receipt.blocked_targets += 1;
            receipt.limitations.push(
                "A storage target could not be cleaned; retry after restoring storage/key access."
                    .into(),
            );
            "blocked"
        }
    };
    save(&mut conn, job.id, final_state, &receipt)?;
    Ok(())
}

#[derive(QueryableByName)]
struct TextRow {
    #[diesel(sql_type=diesel::sql_types::Text)]
    record_id: String,
    #[diesel(sql_type=diesel::sql_types::Text)]
    value: String,
}
fn cleanup(
    state: &AppState,
    conn: &mut PgConnection,
    rules: &Redactor,
    receipt: &mut Receipt,
    job: Uuid,
    target: Uuid,
    owner: Option<Uuid>,
) -> Result<(), AppError> {
    // Only content-bearing fields, never identity/credential/cryptographic
    // tables. Operational identity and authentication rows are not archives.
    for (table, pk, field, json) in [
        ("messages", "id", "content", false),
        ("pending_inputs", "id", "content", false),
        ("session_continuations", "id", "prompt", false),
        ("session_continuations", "id", "source_message", false),
        ("session_continuations", "id", "last_error", false),
        ("scheduled_tasks", "id", "prompt", false),
        ("session_edit_stack_items", "id", "title", false),
        ("session_edit_stack_items", "id", "body", false),
        ("session_edit_stack_items", "id", "context", false),
        ("session_attention", "session_id", "preview", false),
        ("sessions", "id", "session_name", false),
        ("sessions", "id", "claude_args", true),
    ] {
        let mut after = String::new();
        loop {
            let owned = match table {
                "scheduled_tasks" | "sessions" => "user_id=$2",
                _ => "session_id IN (SELECT id FROM sessions WHERE user_id=$2)",
            };
            let filter = if owner.is_some() {
                format!(" AND {owned}")
            } else {
                String::new()
            };
            let query=format!("SELECT {pk}::text AS record_id, left({field}::text,4194305) AS value FROM {table} WHERE {pk}::text > $1 AND {field} IS NOT NULL{filter} ORDER BY {pk}::text LIMIT 8");
            let rows = if let Some(user) = owner {
                diesel::sql_query(query)
                    .bind::<diesel::sql_types::Text, _>(&after)
                    .bind::<diesel::sql_types::Uuid, _>(user)
                    .load::<TextRow>(conn)?
            } else {
                diesel::sql_query(query)
                    .bind::<diesel::sql_types::Text, _>(&after)
                    .load::<TextRow>(conn)?
            };
            if rows.is_empty() {
                break;
            }
            for row in rows {
                after = row.record_id.clone();
                receipt.bytes_scanned += row.value.len() as i64;
                let sanitized = if matches!(table, "messages" | "pending_inputs") {
                    rules.redact_agent_content(&row.value)
                } else {
                    rules.redact_content(&row.value)
                };
                if sanitized != row.value {
                    let sql=format!("UPDATE {table} SET {field}=$1{} WHERE {pk}::text=$2 AND left({field}::text,4194305)=$3",if json{"::jsonb"}else{""});
                    let value =
                        if json && serde_json::from_str::<serde_json::Value>(&sanitized).is_err() {
                            serde_json::to_string(&sanitized)
                                .map_err(|_| AppError::Internal("Cannot redact record".into()))?
                        } else {
                            sanitized
                        };
                    receipt.records_redacted += diesel::sql_query(sql)
                        .bind::<diesel::sql_types::Text, _>(&value)
                        .bind::<diesel::sql_types::Text, _>(&row.record_id)
                        .bind::<diesel::sql_types::Text, _>(&row.value)
                        .execute(conn)? as i64;
                }
            }
            save(conn, job, "redacting", receipt)?;
        }
    }
    if let Some(runtime) = &state.archive {
        let mut warnings = Vec::new();
        let rows = archive_format::scan::collect_rows(&runtime.store, &mut |_| warnings.push(()))
            .map_err(|_| AppError::ServiceUnavailable("Archive inventory unavailable"))?;
        receipt.blocked_targets += warnings.len() as i64;
        for row in rows {
            if owner.is_some_and(|owner| owner != row.manifest.user_id) {
                continue;
            }
            // Same lock order as archival: process manifest lock, then DB
            // session lease. The DB lock also fences other backend processes.
            let _manifest_guard = runtime
                .manifest_write_lock
                .lock()
                .map_err(|_| AppError::ServiceUnavailable("Archive writer unavailable"))?;
            let result = conn.transaction::<_, AppError, _>(|conn| {
                archive_session_lock(conn, row.manifest.session_id)?;
                let latest = runtime
                    .store
                    .get_session_manifest(row.manifest.user_id, row.manifest.session_id)
                    .map_err(|_| AppError::ServiceUnavailable("Archive manifest unavailable"))?
                    .ok_or(AppError::NotFound("Archive manifest not found"))?;
                rewrite_archive(state, runtime, conn, latest, rules, receipt, target)
            });
            if result.is_err() {
                receipt.blocked_targets += 1;
            }
            save(conn, job, "redacting", receipt)?;
        }
        runtime.warm_scan_cache();
    }
    // Buffered pending inputs can otherwise replay a pre-cleanup copy. The
    // send boundary independently re-sanitizes; connections are recycled to
    // discard browser-held server caches and force sanitized history replay.
    receipt.limitations.push("Opaque images/video and previously downloaded client copies cannot be certified by text matching; review or delete those assets separately.".into());
    Ok(())
}

pub fn archive_session_lock(conn: &mut PgConnection, session: Uuid) -> Result<(), AppError> {
    diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind::<diesel::sql_types::Text, _>(format!("portal-archive-{session}"))
        .execute(conn)?;
    Ok(())
}

pub(crate) fn bounded_line(reader: &mut dyn BufRead) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    let count = reader
        .take(MAX_RECORD_BYTES + 1)
        .read_until(b'\n', &mut bytes)?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(std::io::Error::other(
            "Archive record exceeds cleanup limit",
        ));
    }
    Ok(Some(bytes))
}
fn rewrite_archive(
    state: &AppState,
    runtime: &crate::archive::ArchiveRuntime,
    conn: &mut PgConnection,
    mut manifest: archive_format::SessionArchiveManifest,
    rules: &Redactor,
    receipt: &mut Receipt,
    target: Uuid,
) -> Result<(), AppError> {
    if redaction::snapshot(conn)?.generation != rules.generation {
        return Err(AppError::Conflict(
            "Secret rules changed during cleanup; retry the job",
        ));
    }
    let object = archive_format::transcript_key(manifest.user_id, manifest.session_id);
    let coverage_key = format!("secret/{target}/{object}");
    use crate::schema::secret_versions;
    let version_count = secret_versions::table
        .filter(secret_versions::secret_id.eq(target))
        .count()
        .get_result::<i64>(conn)?;
    // Version count changes only on append; low bits version the match policy.
    let coverage_generation = version_count.saturating_mul(16) + 1;
    let blocked_before = receipt.blocked_targets;
    let generation = manifest_fingerprint(runtime, &manifest)?;
    use crate::schema::secret_coverage as coverage;
    if coverage::table
        .find(&coverage_key)
        .filter(coverage::rule_generation.eq(coverage_generation))
        .filter(coverage::object_generation.eq(&generation))
        .count()
        .get_result::<i64>(conn)?
        > 0
    {
        return Ok(());
    }
    let mut reader = runtime
        .store
        .transcript_reader(manifest.user_id, manifest.session_id)
        .map_err(|_| AppError::ServiceUnavailable("Archive transcript unavailable"))?;
    let mut file = tempfile::tempfile()
        .map_err(|_| AppError::ServiceUnavailable("Cleanup temporary storage unavailable"))?;
    let mut encoder = zstd::Encoder::new(&mut file, 3)
        .map_err(|_| AppError::ServiceUnavailable("Cleanup compressor unavailable"))?;
    let mut changed = false;
    let mut bytes = 0u64;
    let mut scanned = 0u64;
    while let Some(raw) = bounded_line(reader.as_mut())
        .map_err(|_| AppError::ServiceUnavailable("Archive record exceeds cleanup budget"))?
    {
        scanned += raw.len() as u64;
        if scanned > MAX_ARCHIVE_BYTES {
            return Err(AppError::ServiceUnavailable(
                "Archive exceeds cleanup disk budget",
            ));
        }
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        redaction::validate_json_budget(&raw)?;
        let mut line: archive_format::ArchiveMessageLine = serde_json::from_slice(&raw)
            .map_err(|_| AppError::ServiceUnavailable("Invalid archive record"))?;
        rules.redact_agent_json(&mut line.content);
        let serialized = serde_json::to_vec(&line)
            .map_err(|_| AppError::Internal("Cannot encode archive record".into()))?;
        changed |= serialized.as_slice() != raw.strip_suffix(b"\n").unwrap_or(&raw);
        encoder
            .write_all(&serialized)
            .and_then(|_| encoder.write_all(b"\n"))
            .map_err(|_| AppError::ServiceUnavailable("Cleanup temporary storage full"))?;
        bytes += serialized.len() as u64 + 1;
    }
    encoder
        .finish()
        .map_err(|_| AppError::ServiceUnavailable("Cleanup compression failed"))?;
    receipt.bytes_scanned += scanned as i64;
    if clean_media(
        state,
        runtime,
        conn,
        &mut manifest,
        rules,
        receipt,
        (target, coverage_generation),
    )? {
        changed = true;
    }
    let old_meta = serde_json::to_value(&manifest)
        .map_err(|_| AppError::Internal("Cannot read archive metadata".into()))?;
    rules.redact_manifest(&mut manifest);
    let metadata_changed = serde_json::to_value(&manifest)
        .map_err(|_| AppError::Internal("Cannot encode archive metadata".into()))?
        != old_meta;
    redaction::lock_generation(conn)?;
    if redaction::snapshot(conn)?.generation != rules.generation {
        return Err(AppError::Conflict(
            "Secret rules changed during cleanup; retry the job",
        ));
    }
    if changed || metadata_changed {
        manifest.archived_at = Utc::now().naive_utc();
        if let Some(info) = &mut manifest.transcript {
            info.bytes = file
                .metadata()
                .map_err(|_| AppError::ServiceUnavailable("Cleanup spool unavailable"))?
                .len();
        }
        file.rewind()
            .map_err(|_| AppError::ServiceUnavailable("Cleanup spool unavailable"))?;
        runtime
            .store
            .put_session_archive_file(&manifest, Some(&mut file))
            .map_err(|_| AppError::ServiceUnavailable("Cannot publish cleaned archive"))?;
        receipt.objects_rewritten += 1;
    }
    if matches!(
        runtime.config.backend,
        archive_format::ArchiveBackendConfig::S3 { .. }
    ) {
        let handle = tokio::runtime::Handle::current();
        for key in [
            &object,
            &archive_format::manifest_key(manifest.user_id, manifest.session_id),
        ] {
            if handle
                .block_on(super::archive_versions::purge_superseded(
                    &runtime.config.backend,
                    key,
                ))
                .is_err()
            {
                receipt.blocked_targets += 1;
                receipt.limitations.push("Historical S3 object versions could not be erased; object lock or permissions may block deletion.".into());
            }
        }
    }
    let _ = bytes;
    let new_generation = manifest_fingerprint(runtime, &manifest)?;
    if receipt.blocked_targets > blocked_before {
        return Ok(());
    }
    diesel::insert_into(coverage::table)
        .values((
            coverage::object_id.eq(&coverage_key),
            coverage::rule_generation.eq(coverage_generation),
            coverage::object_generation.eq(new_generation.clone()),
        ))
        .on_conflict(coverage::object_id)
        .do_update()
        .set((
            coverage::rule_generation.eq(coverage_generation),
            coverage::object_generation.eq(new_generation),
        ))
        .execute(conn)?;
    Ok(())
}

pub(crate) fn scan_media(
    reader: &mut dyn std::io::Read,
    rules: &Redactor,
) -> std::io::Result<(bool, u64)> {
    let mut buffer = vec![0; 64 * 1024];
    let mut overlap = Vec::new();
    let mut total = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok((false, total));
        }
        total += count as u64;
        overlap.extend_from_slice(&buffer[..count]);
        if rules.contains_bytes(&overlap) {
            return Ok((true, total));
        }
        if overlap.len() > 256 * 1024 {
            overlap.drain(..overlap.len() - 256 * 1024);
        }
    }
}
fn clean_media(
    state: &AppState,
    runtime: &crate::archive::ArchiveRuntime,
    conn: &mut PgConnection,
    manifest: &mut archive_format::SessionArchiveManifest,
    rules: &Redactor,
    receipt: &mut Receipt,
    policy: (Uuid, i64),
) -> Result<bool, AppError> {
    let (target, coverage_generation) = policy;
    use crate::schema::secret_coverage as coverage;
    let Some(entries) = manifest.media.take() else {
        return Ok(false);
    };
    let mut kept = Vec::with_capacity(entries.len());
    let mut changed = false;
    for entry in entries {
        let key = archive_format::media_key(manifest.user_id, manifest.session_id, entry.media_id);
        let coverage_key = format!("secret/{target}/{key}");
        let blocked_before = receipt.blocked_targets;
        let meta_key =
            archive_format::media_meta_key(manifest.user_id, manifest.session_id, entry.media_id);
        let tombstone = format!("deleted-media/{}", entry.media_id);
        let removed = coverage::table
            .find(&tombstone)
            .count()
            .get_result::<i64>(conn)?
            > 0;
        let object_generation = format!("{}:{}", entry.bytes, entry.uploaded_at);
        let covered = coverage::table
            .find(&coverage_key)
            .filter(coverage::rule_generation.eq(coverage_generation))
            .filter(coverage::object_generation.eq(&object_generation))
            .count()
            .get_result::<i64>(conn)?
            > 0;
        if covered && !removed {
            kept.push(entry);
            continue;
        }
        let mut contaminated = removed;
        if !removed {
            match runtime.store.object_reader(&key, 512 * 1024 * 1024) {
                Ok(Some(mut reader)) => {
                    let (found, bytes) = scan_media(reader.as_mut(), rules).map_err(|_| {
                        AppError::ServiceUnavailable("Cannot inspect archive media")
                    })?;
                    contaminated = found;
                    receipt.bytes_scanned += bytes as i64;
                }
                Ok(None) => {}
                Err(_) => {
                    receipt.blocked_targets += 1;
                    kept.push(entry);
                    continue;
                }
            }
        }
        if contaminated {
            diesel::insert_into(coverage::table)
                .values((
                    coverage::object_id.eq(&tombstone),
                    coverage::rule_generation.eq(rules.generation),
                    coverage::object_generation.eq("removed"),
                ))
                .on_conflict(coverage::object_id)
                .do_nothing()
                .execute(conn)?;
            {
                let mut suppressed = runtime
                    .suppressed_media
                    .write()
                    .map_err(|_| AppError::ServiceUnavailable("Media cleanup state unavailable"))?;
                if suppressed.len() < 4096 {
                    suppressed.insert(entry.media_id);
                }
            }
            state.image_store.invalidate(&entry.media_id);
            state.media_store.remove(&entry.media_id);
            for object in [&key, &meta_key] {
                runtime.store.delete_object(object).map_err(|_| {
                    AppError::ServiceUnavailable("Cannot remove contaminated media")
                })?;
                if matches!(
                    runtime.config.backend,
                    archive_format::ArchiveBackendConfig::S3 { .. }
                ) && tokio::runtime::Handle::current()
                    .block_on(super::archive_versions::delete_all_versions(
                        &runtime.config.backend,
                        object,
                    ))
                    .is_err()
                {
                    receipt.blocked_targets += 1;
                }
            }
            receipt.objects_rewritten += 1;
            changed = true;
        } else {
            if let Some(bytes) = runtime
                .store
                .get_object(&meta_key)
                .map_err(|_| AppError::ServiceUnavailable("Cannot inspect media metadata"))?
            {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| AppError::ServiceUnavailable("Invalid media metadata"))?;
                let before = value.clone();
                rules.redact_json(&mut value);
                if value != before {
                    runtime
                        .store
                        .put_object(
                            &meta_key,
                            serde_json::to_vec(&value).map_err(|_| {
                                AppError::Internal("Cannot encode media metadata".into())
                            })?,
                        )
                        .map_err(|_| AppError::ServiceUnavailable("Cannot clean media metadata"))?;
                    receipt.objects_rewritten += 1;
                }
            }
            if matches!(
                runtime.config.backend,
                archive_format::ArchiveBackendConfig::S3 { .. }
            ) {
                for object in [&key, &meta_key] {
                    if tokio::runtime::Handle::current()
                        .block_on(super::archive_versions::purge_superseded(
                            &runtime.config.backend,
                            object,
                        ))
                        .is_err()
                    {
                        receipt.blocked_targets += 1;
                    }
                }
            }
            if receipt.blocked_targets == blocked_before {
                diesel::insert_into(coverage::table)
                    .values((
                        coverage::object_id.eq(&coverage_key),
                        coverage::rule_generation.eq(coverage_generation),
                        coverage::object_generation.eq(&object_generation),
                    ))
                    .on_conflict(coverage::object_id)
                    .do_update()
                    .set((
                        coverage::rule_generation.eq(coverage_generation),
                        coverage::object_generation.eq(&object_generation),
                    ))
                    .execute(conn)?;
            }
            kept.push(entry);
        }
    }
    manifest.media = Some(kept);
    Ok(changed)
}

pub fn media_removed(conn: &mut PgConnection, id: Uuid) -> Result<bool, AppError> {
    use crate::schema::secret_coverage as coverage;
    Ok(coverage::table
        .find(format!("deleted-media/{id}"))
        .count()
        .get_result::<i64>(conn)?
        > 0)
}
pub fn ensure_media_bytes(
    state: &AppState,
    id: Uuid,
    owner: Uuid,
    bytes: &[u8],
) -> Result<(), AppError> {
    let mut conn = state.conn()?;
    if media_removed(&mut conn, id)?
        || redaction::snapshot_for_user(&mut conn, owner)?.contains_bytes(bytes)
    {
        return Err(AppError::NotFound(
            "Media unavailable after secret redaction",
        ));
    }
    Ok(())
}
pub fn ensure_media_file(
    state: &AppState,
    id: Uuid,
    owner: Uuid,
    path: &std::path::Path,
) -> Result<(), AppError> {
    let mut conn = state.conn()?;
    if media_removed(&mut conn, id)? {
        return Err(AppError::NotFound(
            "Media unavailable after secret redaction",
        ));
    }
    let rules = redaction::snapshot_for_user(&mut conn, owner)?;
    if rules.is_empty() {
        return Ok(());
    }
    let mut file = std::fs::File::open(path).map_err(|_| AppError::NotFound("Media not found"))?;
    if file
        .metadata()
        .map_err(|_| AppError::NotFound("Media not found"))?
        .len()
        > 512 * 1024 * 1024
    {
        return Err(AppError::ServiceUnavailable(
            "Media exceeds safe inspection budget",
        ));
    }
    if scan_media(&mut file, &rules)
        .map_err(|_| AppError::ServiceUnavailable("Media inspection unavailable"))?
        .0
    {
        return Err(AppError::NotFound(
            "Media unavailable after secret redaction",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "expunge/cleanup_tests.rs"]
mod tests;

fn manifest_fingerprint(
    runtime: &crate::archive::ArchiveRuntime,
    manifest: &archive_format::SessionArchiveManifest,
) -> Result<String, AppError> {
    let key = openssl::pkey::PKey::hmac(&runtime.cleanup_cache_key)
        .map_err(|_| AppError::ServiceUnavailable("Cleanup fingerprint unavailable"))?;
    let mut signer = openssl::sign::Signer::new(openssl::hash::MessageDigest::sha256(), &key)
        .map_err(|_| AppError::ServiceUnavailable("Cleanup fingerprint unavailable"))?;
    let bytes = serde_json::to_vec(manifest)
        .map_err(|_| AppError::Internal("Cannot encode archive metadata".into()))?;
    signer
        .update(&bytes)
        .map_err(|_| AppError::ServiceUnavailable("Cleanup fingerprint unavailable"))?;
    Ok(hex::encode(signer.sign_to_vec().map_err(|_| {
        AppError::ServiceUnavailable("Cleanup fingerprint unavailable")
    })?))
}

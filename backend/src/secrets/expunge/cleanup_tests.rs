//! Real vault requests, isolated SQL schemas, and compressed local archives.
//! Assertions inspect persisted content/receipts, not merely cleanup counters.
use super::*;
use crate::handlers::secrets::tests::{credential, request, Database};
use archive_format::{
    ArchiveMessageLine, ArchiveTranscriptInfo, SessionArchiveBundle, SessionArchiveManifest,
};
use axum::{http::StatusCode, Router};
use shared::api::{
    SecretCreateRequest, SecretGrantRequest, SecretMetadata, SecretNamespace, SecretRight,
    SecretValue, SecretWriteRequest,
};
use zeroize::Zeroizing;

struct Actor {
    id: Uuid,
    token: String,
}
struct Fixture {
    state: Arc<AppState>,
    router: Router,
    owner: Actor,
    other: Actor,
    admin: Actor,
    _directory: tempfile::TempDir,
    _database: Database,
}
impl Fixture {
    fn new() -> Option<Self> {
        let database = Database::new()?;
        let directory = tempfile::tempdir().unwrap();
        let mut state = crate::test_support::test_app_state(database.pool.clone());
        state.secrets = Arc::new(super::super::SecretsService::new(Some(
            super::super::crypto::EnvelopeCrypto {
                key: Arc::new(Zeroizing::new(vec![73; 32])),
            },
        )));
        let runtime = Arc::new(
            crate::archive::ArchiveRuntime::new(archive_format::ArchiveConfig {
                backend: archive_format::ArchiveBackendConfig::Local {
                    root: directory.path().to_owned(),
                },
                transcripts: true,
                media: true,
            })
            .unwrap(),
        );
        *runtime.media_db_pool.write().unwrap() = Some(database.pool.clone());
        state.archive = Some(runtime);
        let state = Arc::new(state);
        let actor = |label: &str| {
            let id = crate::test_support::insert_user(&mut state.conn().unwrap(), label).id;
            Actor {
                id,
                token: credential(&state, id, label),
            }
        };
        let owner = actor("expunge-owner");
        let other = actor("expunge-other");
        let admin = actor("expunge-admin");
        diesel::update(crate::schema::users::table.find(admin.id))
            .set(crate::schema::users::is_admin.eq(true))
            .execute(&mut state.conn().unwrap())
            .unwrap();
        let router = crate::handlers::secrets::router(state.clone())
            .with_state(state.clone())
            .layer(tower_cookies::CookieManagerLayer::new());
        Some(Self {
            state,
            router,
            owner,
            other,
            admin,
            _directory: directory,
            _database: database,
        })
    }
    fn runtime(&self) -> &crate::archive::ArchiveRuntime {
        self.state.archive.as_ref().unwrap()
    }
    async fn create(&self, actor: &Actor, name: &str, value: &str) -> SecretMetadata {
        let body = SecretCreateRequest {
            namespace: SecretNamespace::Personal,
            name: name.into(),
            description: String::new(),
            value: SecretValue(value.into()),
        };
        let (status, result) = request(
            &self.router,
            &actor.token,
            "POST",
            "/api/secrets",
            serde_json::to_value(body).unwrap(),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "create status {status}: {result}");
        serde_json::from_value(result).unwrap()
    }
    async fn rotate(&self, actor: &Actor, secret: &SecretMetadata, value: &str) -> SecretMetadata {
        let body = SecretWriteRequest {
            expected_generation: secret.generation,
            value: SecretValue(value.into()),
        };
        let (status, result) = request(
            &self.router,
            &actor.token,
            "POST",
            &format!("/api/secrets/{}/versions", secret.id),
            serde_json::to_value(body).unwrap(),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "rotate status {status}: {result}");
        serde_json::from_value(result).unwrap()
    }
    async fn enqueue(&self, actor: &Actor, secret: Uuid, global: bool) -> SecretExpungeJob {
        let (status, result) = request(
            &self.router,
            &actor.token,
            "POST",
            &format!("/api/secrets/{secret}/expunge"),
            serde_json::to_value(SecretExpungeRequest {
                all_versions: true,
                global,
            })
            .unwrap(),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "enqueue status {status}: {result}");
        serde_json::from_value(result).unwrap()
    }
    async fn receipt(&self, actor: &Actor, job: Uuid) -> SecretExpungeJob {
        let (status, result) = request(
            &self.router,
            &actor.token,
            "GET",
            &format!("/api/secrets/jobs/{job}"),
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "job status {status}: {result}");
        serde_json::from_value(result).unwrap()
    }
    async fn run_next(&self) {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || super::run_next(&state))
            .await
            .unwrap()
            .unwrap();
    }
    fn archive(
        &self,
        owner: Uuid,
        contents: &[&str],
    ) -> (SessionArchiveManifest, Vec<ArchiveMessageLine>) {
        let session = crate::test_support::insert_session(
            &mut self.state.conn().unwrap(),
            owner,
            "Historical transcript",
        );
        let time = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let lines = contents
            .iter()
            .enumerate()
            .map(|(index, text)| ArchiveMessageLine {
                source_created_at: if index == 0 {
                    None
                } else {
                    Some(time + chrono::Duration::seconds(index as i64))
                },
                id: Uuid::new_v4(),
                role: "assistant".into(),
                created_at: time.naive_utc() + chrono::Duration::microseconds(index as i64),
                agent_type: "codex".into(),
                content: serde_json::json!({"type":"message","text":text}),
            })
            .collect::<Vec<_>>();
        let mut raw = Vec::new();
        for line in &lines {
            serde_json::to_writer(&mut raw, line).unwrap();
            raw.push(b'\n');
        }
        let mut manifest = archive_format::scan::test_support::manifest(
            owner,
            session.id,
            "archive@example.invalid",
            "Historical transcript",
            "codex",
            time.naive_utc(),
        );
        manifest.transcript = Some(ArchiveTranscriptInfo {
            object_key: archive_format::transcript_key(owner, session.id),
            compression: archive_format::TRANSCRIPT_COMPRESSION.into(),
            message_count: lines.len() as i64,
            bytes: archive_format::zstd_encode(&raw).unwrap().len() as u64,
        });
        self.runtime()
            .store
            .put_session_archive(&SessionArchiveBundle {
                manifest: manifest.clone(),
                transcript_ndjson: Some(raw),
            })
            .unwrap();
        (manifest, lines)
    }
    fn lines(&self, manifest: &SessionArchiveManifest) -> Vec<ArchiveMessageLine> {
        self.runtime()
            .store
            .read_transcript_lines(manifest.user_id, manifest.session_id)
            .unwrap()
            .unwrap()
    }
    fn archive_bytes(&self, manifest: &SessionArchiveManifest) -> Vec<u8> {
        self.runtime()
            .store
            .get_object(&archive_format::transcript_key(
                manifest.user_id,
                manifest.session_id,
            ))
            .unwrap()
            .unwrap()
    }
    fn hot(&self, owner: Uuid, text: &str) -> Uuid {
        let mut conn = self.state.conn().unwrap();
        let session = crate::test_support::insert_session(&mut conn, owner, "Historical hot rows");
        use crate::schema::messages;
        diesel::insert_into(messages::table)
            .values(&crate::models::NewMessage {
                session_id: session.id,
                role: "assistant".into(),
                content: text.into(),
                user_id: owner,
                agent_type: "codex".into(),
                provenance_kind: None,
                provenance_session_id: None,
                provenance_agent_type: None,
            })
            .returning(messages::id)
            .get_result(&mut conn)
            .unwrap()
    }
    fn hot_content(&self, id: Uuid) -> String {
        use crate::schema::messages;
        messages::table
            .find(id)
            .select(messages::content)
            .first(&mut self.state.conn().unwrap())
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_epoch_jobs_have_target_specific_coverage_and_unchanged_retry_does_not_rewrite() {
    let Some(f) = Fixture::new() else {
        return;
    };
    let a = f
        .create(&f.owner, "TOKEN_A", "alpha-private-history-token")
        .await;
    let b = f
        .create(&f.owner, "TOKEN_B", "bravo-private-history-token")
        .await;
    let (manifest, original) = f.archive(
        f.owner.id,
        &[
            "alpha-private-history-token and bravo-private-history-token",
            "preserve transcript order",
        ],
    );
    let ra = f
        .state
        .secrets
        .redactor_for_secret(&f.state.db_pool, a.id)
        .await
        .unwrap();
    let rb = f
        .state
        .secrets
        .redactor_for_secret(&f.state.db_pool, b.id)
        .await
        .unwrap();
    assert_eq!(ra.generation, rb.generation);
    let job_a = f.enqueue(&f.owner, a.id, false).await;
    f.run_next().await;
    let after_a = f.lines(&manifest);
    assert!(!after_a[0]
        .content
        .to_string()
        .contains("alpha-private-history-token"));
    assert!(after_a[0]
        .content
        .to_string()
        .contains("bravo-private-history-token"));
    let job_b = f.enqueue(&f.owner, b.id, false).await;
    f.run_next().await;
    let after_b = f.lines(&manifest);
    assert!(!after_b[0]
        .content
        .to_string()
        .contains("bravo-private-history-token"));
    assert_eq!(
        after_b.iter().map(|line| line.id).collect::<Vec<_>>(),
        original.iter().map(|line| line.id).collect::<Vec<_>>()
    );
    assert_eq!(
        f.receipt(&f.owner, job_a.id).await.state,
        SecretExpungeState::AwaitingBackupExpiry
    );
    assert_eq!(
        f.receipt(&f.owner, job_b.id).await.state,
        SecretExpungeState::AwaitingBackupExpiry
    );
    use crate::schema::secret_coverage as coverage;
    let keys = coverage::table
        .select(coverage::object_id)
        .load::<String>(&mut f.state.conn().unwrap())
        .unwrap();
    let object = archive_format::transcript_key(manifest.user_id, manifest.session_id);
    assert!(keys.contains(&format!("secret/{}/{object}", a.id)));
    assert!(keys.contains(&format!("secret/{}/{object}", b.id)));
    let before = f.archive_bytes(&manifest);
    let manifest_before = f
        .runtime()
        .store
        .get_object(&archive_format::manifest_key(
            manifest.user_id,
            manifest.session_id,
        ))
        .unwrap()
        .unwrap();
    let retry = f.enqueue(&f.owner, b.id, false).await;
    f.run_next().await;
    let receipt = f.receipt(&f.owner, retry.id).await;
    assert_eq!(receipt.objects_rewritten, 0);
    assert_eq!(f.archive_bytes(&manifest), before);
    assert_eq!(
        f.runtime()
            .store
            .get_object(&archive_format::manifest_key(
                manifest.user_id,
                manifest.session_id
            ))
            .unwrap()
            .unwrap(),
        manifest_before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compressed_v1_archive_redacts_every_retained_target_version_preserving_clocks_and_other_secret(
) {
    let Some(f) = Fixture::new() else {
        return;
    };
    let target = f
        .create(&f.owner, "ROTATING", "historical-target-version-one")
        .await;
    let target = f
        .rotate(&f.owner, &target, "historical-target-version-two")
        .await;
    let other = f
        .create(&f.owner, "OTHER", "unrelated-private-secret-kept")
        .await;
    let (manifest, original) = f.archive(
        f.owner.id,
        &[
            "historical-target-version-one",
            "historical-target-version-two and unrelated-private-secret-kept",
            "clean final message",
        ],
    );
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(
        manifest.transcript.as_ref().unwrap().compression,
        archive_format::TRANSCRIPT_COMPRESSION
    );
    let job = f.enqueue(&f.owner, target.id, false).await;
    f.run_next().await;
    let cleaned = f.lines(&manifest);
    assert_eq!(cleaned.len(), original.len());
    for (before, after) in original.iter().zip(&cleaned) {
        assert_eq!(before.id, after.id);
        assert_eq!(before.created_at, after.created_at);
        assert_eq!(before.source_created_at, after.source_created_at);
        assert_eq!(before.role, after.role);
    }
    let text = serde_json::to_string(&cleaned).unwrap();
    assert!(!text.contains("historical-target-version-one"));
    assert!(!text.contains("historical-target-version-two"));
    assert!(text.contains("unrelated-private-secret-kept"));
    assert_eq!(cleaned[2].content, original[2].content);
    assert_eq!(f.receipt(&f.owner, job.id).await.blocked_targets, 0);
    use crate::schema::secret_versions;
    assert_eq!(
        secret_versions::table
            .filter(secret_versions::secret_id.eq(target.id))
            .count()
            .get_result::<i64>(&mut f.state.conn().unwrap())
            .unwrap(),
        2
    );
    assert_eq!(
        secret_versions::table
            .filter(secret_versions::secret_id.eq(other.id))
            .count()
            .get_result::<i64>(&mut f.state.conn().unwrap())
            .unwrap(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own_cleanup_does_not_touch_another_owner_and_global_cleanup_requires_curator_and_manage() {
    let Some(f) = Fixture::new() else {
        return;
    };
    let secret = f
        .create(&f.owner, "SCOPED", "cross-owner-private-secret-token")
        .await;
    let (own, _) = f.archive(f.owner.id, &["cross-owner-private-secret-token"]);
    let (other, _) = f.archive(f.other.id, &["cross-owner-private-secret-token"]);
    let own_hot = f.hot(f.owner.id, "cross-owner-private-secret-token");
    let other_hot = f.hot(f.other.id, "cross-owner-private-secret-token");
    let other_before = f.archive_bytes(&other);
    let (denied, _) = request(
        &f.router,
        &f.owner.token,
        "POST",
        &format!("/api/secrets/{}/expunge", secret.id),
        serde_json::to_value(SecretExpungeRequest {
            all_versions: true,
            global: true,
        })
        .unwrap(),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    let job = f.enqueue(&f.owner, secret.id, false).await;
    f.run_next().await;
    assert!(!f.lines(&own)[0]
        .content
        .to_string()
        .contains("cross-owner-private-secret-token"));
    assert_eq!(f.archive_bytes(&other), other_before);
    assert!(!f
        .hot_content(own_hot)
        .contains("cross-owner-private-secret-token"));
    assert_eq!(f.hot_content(other_hot), "cross-owner-private-secret-token");
    for actor in [&f.other, &f.admin] {
        let (status, _) = request(
            &f.router,
            &actor.token,
            "GET",
            &format!("/api/secrets/jobs/{}", job.id),
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "another user's private job is not curator-readable by default"
        );
    }
    let grant = SecretGrantRequest {
        expected_generation: secret.generation,
        user_id: f.admin.id,
        rights: vec![SecretRight::Manage],
    };
    let (status, _) = request(
        &f.router,
        &f.owner.token,
        "POST",
        &format!("/api/secrets/{}/grants", secret.id),
        serde_json::to_value(grant).unwrap(),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    assert_eq!(f.receipt(&f.admin, job.id).await.id, job.id);
    let global = f.enqueue(&f.admin, secret.id, true).await;
    f.run_next().await;
    assert!(!f.lines(&other)[0]
        .content
        .to_string()
        .contains("cross-owner-private-secret-token"));
    assert!(!f
        .hot_content(other_hot)
        .contains("cross-owner-private-secret-token"));
    assert_eq!(
        f.receipt(&f.admin, global.id).await.state,
        SecretExpungeState::AwaitingBackupExpiry
    );
    let (read, _) = request(
        &f.router,
        &f.admin.token,
        "POST",
        &format!("/api/secrets/{}/read", secret.id),
        serde_json::json!({"version":null}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(
        read,
        StatusCode::FORBIDDEN,
        "cleanup management must not grant saved-value read"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_policy_refresh_sanitizes_other_owner_stale_buffers_without_using_their_private_rules_for_cleanup(
) {
    let Some(f) = Fixture::new() else {
        return;
    };
    let target = f
        .create(&f.admin, "GLOBAL_TARGET", "admin-global-expunge-target")
        .await;
    f.create(
        &f.other,
        "OTHER_PRIVATE",
        "other-owner-unrelated-private-value",
    )
    .await;
    let (archive, _) = f.archive(
        f.other.id,
        &["admin-global-expunge-target and other-owner-unrelated-private-value"],
    );
    let old_rules = redaction::snapshot_for_user(&mut f.state.conn().unwrap(), f.other.id).unwrap();
    let stale_buffer = serde_json::json!({"text":"admin-global-expunge-target and other-owner-unrelated-private-value"});
    assert_eq!(
        old_rules.redact_str("admin-global-expunge-target"),
        "admin-global-expunge-target"
    );
    let job = f.enqueue(&f.admin, target.id, true).await;
    let fresh = redaction::snapshot_for_user(&mut f.state.conn().unwrap(), f.other.id).unwrap();
    assert!(fresh.generation > old_rules.generation);
    assert_eq!(
        fresh.redact_str("admin-global-expunge-target"),
        redaction::MASK
    );
    let sent: serde_json::Value =
        redaction::sanitize_for_user(&f.state.db_pool, f.other.id, stale_buffer.clone()).unwrap();
    assert!(!sent.to_string().contains("admin-global-expunge-target"));
    assert!(!sent
        .to_string()
        .contains("other-owner-unrelated-private-value"));
    f.run_next().await;
    let persisted = f.lines(&archive)[0].content.to_string();
    assert!(!persisted.contains("admin-global-expunge-target"));
    assert!(
        persisted.contains("other-owner-unrelated-private-value"),
        "target job must not apply unrelated personal rules while rewriting another owner"
    );
    assert_eq!(f.receipt(&f.admin, job.id).await.blocked_targets, 0);
    let (read, _) = request(
        &f.router,
        &f.other.token,
        "POST",
        &format!("/api/secrets/{}/read", target.id),
        serde_json::json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(read, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contaminated_media_is_deleted_as_a_whole_and_durable_tombstone_blocks_late_write_after_restart(
) {
    let Some(f) = Fixture::new() else {
        return;
    };
    let secret = f
        .create(&f.owner, "MEDIA_SECRET", "media-binary-private-token")
        .await;
    let (mut manifest, _) = f.archive(f.owner.id, &["a referenced media attachment"]);
    let media = Uuid::new_v4();
    let mut bytes = vec![0u8; 64 * 1024 - 8];
    bytes.extend_from_slice(b"media-binary-private-token");
    bytes.extend_from_slice(b"\0opaque-image-tail");
    let meta = archive_format::ArchivedMediaMeta {
        media_id: media,
        kind: "image".into(),
        content_type: "image/png".into(),
        filename: Some("image.png".into()),
        bytes: bytes.len() as u64,
        uploaded_at: manifest.archived_at,
    };
    f.runtime()
        .store
        .put_media(manifest.user_id, manifest.session_id, &meta, &bytes)
        .unwrap();
    f.state.image_store.insert_with_id(
        media,
        "image/png",
        bytes.clone(),
        manifest.user_id,
        Some(manifest.session_id),
    );
    manifest.media = Some(vec![archive_format::MediaEntry {
        media_id: media,
        kind: meta.kind.clone(),
        content_type: meta.content_type.clone(),
        bytes: meta.bytes,
        object_key: archive_format::media_key(manifest.user_id, manifest.session_id, media),
        uploaded_at: meta.uploaded_at,
    }]);
    let transcript = f.lines(&manifest);
    f.runtime()
        .store
        .put_object(
            &archive_format::manifest_key(manifest.user_id, manifest.session_id),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    let job = f.enqueue(&f.owner, secret.id, false).await;
    f.run_next().await;
    assert!(f
        .runtime()
        .store
        .get_media_bytes(manifest.user_id, manifest.session_id, media)
        .unwrap()
        .is_none());
    assert!(f
        .runtime()
        .store
        .get_media_meta(manifest.user_id, manifest.session_id, media)
        .unwrap()
        .is_none());
    assert!(f.state.image_store.get(&media).is_none());
    assert!(media_removed(&mut f.state.conn().unwrap(), media).unwrap());
    assert!(f
        .runtime()
        .store
        .get_session_manifest(manifest.user_id, manifest.session_id)
        .unwrap()
        .unwrap()
        .media
        .unwrap()
        .is_empty());
    assert!(f.receipt(&f.owner, job.id).await.objects_rewritten >= 1);
    assert!(
        ensure_media_bytes(&f.state, media, f.other.id, b"clean late payload").is_err(),
        "tombstone survives regardless of content or owner"
    );
    // Restart loses the process-local suppressed set. SQL must still fence a
    // delayed writer even when its new bytes no longer match the value.
    let restarted = crate::archive::ArchiveRuntime::new(f.runtime().config.clone()).unwrap();
    *restarted.media_db_pool.write().unwrap() = Some(f.state.db_pool.clone());
    assert!(!restarted.suppressed_media.read().unwrap().contains(&media));
    crate::handlers::media_archive::write_through(
        &restarted,
        crate::handlers::media_archive::MediaWriteThrough {
            owner_user_id: manifest.user_id,
            session_id: manifest.session_id,
            media_id: media,
            kind: shared::media::MediaKind::Image,
            content_type: "image/png".into(),
            filename: None,
            bytes: b"clean late payload".to_vec(),
        },
    );
    assert!(restarted
        .store
        .get_media_bytes(manifest.user_id, manifest.session_id, media)
        .unwrap()
        .is_none());
    assert!(restarted
        .store
        .get_media_meta(manifest.user_id, manifest.session_id, media)
        .unwrap()
        .is_none());
    // Transcript text itself stays usable; deleting a blob must not truncate it.
    assert_eq!(
        serde_json::to_value(f.lines(&manifest)).unwrap(),
        serde_json::to_value(transcript).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_and_deep_archive_records_remain_unchanged_and_block_job_completion() {
    for deep in [false, true] {
        let Some(f) = Fixture::new() else {
            return;
        };
        let secret = f
            .create(&f.owner, "UNSAFE_ARCHIVE", "unsafe-record-private-secret")
            .await;
        let (manifest, _) = f.archive(f.owner.id, &["placeholder"]);
        let mut unsafe_line = f.lines(&manifest).remove(0);
        unsafe_line.content = if deep {
            let mut value = serde_json::Value::String("unsafe-record-private-secret".into());
            for _ in 0..80 {
                value = serde_json::Value::Array(vec![value]);
            }
            value
        } else {
            serde_json::Value::String(format!(
                "unsafe-record-private-secret{}",
                "x".repeat(MAX_RECORD_BYTES as usize)
            ))
        };
        let mut raw = serde_json::to_vec(&unsafe_line).unwrap();
        raw.push(b'\n');
        let key = archive_format::transcript_key(manifest.user_id, manifest.session_id);
        let original = archive_format::zstd_encode(&raw).unwrap();
        f.runtime()
            .store
            .put_object(&key, original.clone())
            .unwrap();
        let job = f.enqueue(&f.owner, secret.id, false).await;
        f.run_next().await;
        let receipt = f.receipt(&f.owner, job.id).await;
        assert_eq!(receipt.state, SecretExpungeState::Blocked);
        assert!(receipt.blocked_targets > 0);
        assert_eq!(
            f.runtime().store.get_object(&key).unwrap().unwrap(),
            original
        );
        use crate::schema::secret_coverage as coverage;
        assert_eq!(
            coverage::table
                .find(format!("secret/{}/{key}", secret.id))
                .count()
                .get_result::<i64>(&mut f.state.conn().unwrap())
                .unwrap(),
            0,
            "blocked records must not receive successful coverage"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_approval_cannot_expand_to_a_private_version_appended_before_the_worker_runs() {
    let Some(f) = Fixture::new() else {
        return;
    };
    let target = f
        .create(
            &f.owner,
            "APPROVAL_SCOPE",
            "curator-approved-retained-version",
        )
        .await;
    let (status, granted) = request(
        &f.router,
        &f.owner.token,
        "POST",
        &format!("/api/secrets/{}/grants", target.id),
        serde_json::to_value(SecretGrantRequest {
            expected_generation: target.generation,
            user_id: f.admin.id,
            rights: vec![SecretRight::Manage],
        })
        .unwrap(),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    let target: SecretMetadata = serde_json::from_value(granted).unwrap();
    let (other_archive, _) = f.archive(f.other.id, &["a harmless historical archive record"]);
    let archive_before = f.archive_bytes(&other_archive);
    let other_hot = f.hot(f.other.id, "a harmless historical hot record");
    let hot_before = f.hot_content(other_hot);
    let approved = f.enqueue(&f.admin, target.id, true).await;
    // An unprivileged owner can rotate their key but cannot broaden a previous
    // curator approval into a cross-user one-character censorship rule.
    let rotated = f.rotate(&f.owner, &target, "a").await;
    assert_eq!(rotated.current_version, 2);
    assert_eq!(
        redaction::snapshot_for_user(&mut f.state.conn().unwrap(), f.owner.id)
            .unwrap()
            .redact_str("a"),
        redaction::MASK
    );
    assert_eq!(
        redaction::snapshot_for_user(&mut f.state.conn().unwrap(), f.other.id)
            .unwrap()
            .redact_str("a"),
        "a"
    );
    f.run_next().await;
    let blocked = f.receipt(&f.admin, approved.id).await;
    assert_eq!(blocked.state, SecretExpungeState::Blocked);
    assert!(blocked.blocked_targets > 0);
    assert!(blocked
        .limitations
        .iter()
        .any(|message| message.contains("curator")));
    assert_eq!(blocked.records_redacted, 0);
    assert_eq!(blocked.objects_rewritten, 0);
    assert_eq!(f.hot_content(other_hot), hot_before);
    assert_eq!(f.archive_bytes(&other_archive), archive_before);
    assert_eq!(
        redaction::snapshot_for_user(&mut f.state.conn().unwrap(), f.other.id)
            .unwrap()
            .redact_str("a"),
        "a"
    );
    // Self-scoped jobs intentionally use all current retained versions: the
    // approval freeze protects other owners, not the requesting owner's data.
    let own_hot = f.hot(f.owner.id, "a self-owned record");
    let own = f.enqueue(&f.owner, target.id, false).await;
    f.run_next().await;
    assert!(f.hot_content(own_hot).contains(redaction::MASK));
    assert_eq!(
        f.receipt(&f.owner, own.id).await.state,
        SecretExpungeState::AwaitingBackupExpiry
    );
    assert_eq!(f.hot_content(other_hot), hot_before);
    assert_eq!(f.archive_bytes(&other_archive), archive_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_only_metadata_and_media_changes_invalidate_coverage_without_transcript_changes() {
    let Some(f) = Fixture::new() else {
        return;
    };
    let needle = "manifest-cache-private-secret";
    let target = f.create(&f.owner, "MANIFEST_CACHE", needle).await;
    let (original, original_lines) = f.archive(f.owner.id, &["clean unchanged transcript"]);
    // A preexisting blob may be discovered and attached to the manifest later.
    let media_id = Uuid::new_v4();
    let media_bytes = format!("opaque image bytes with {needle}").into_bytes();
    let meta = archive_format::ArchivedMediaMeta {
        media_id,
        kind: "image".into(),
        content_type: "image/png".into(),
        filename: None,
        bytes: media_bytes.len() as u64,
        uploaded_at: original.archived_at,
    };
    f.runtime()
        .store
        .put_media(original.user_id, original.session_id, &meta, &media_bytes)
        .unwrap();
    let first = f.enqueue(&f.owner, target.id, false).await;
    f.run_next().await;
    assert_eq!(f.receipt(&f.owner, first.id).await.blocked_targets, 0);
    let key = archive_format::manifest_key(original.user_id, original.session_id);
    let mut metadata_changed = f
        .runtime()
        .store
        .get_session_manifest(original.user_id, original.session_id)
        .unwrap()
        .unwrap();
    let old_clock = metadata_changed.archived_at;
    let old_transcript = metadata_changed.transcript.clone();
    metadata_changed.session_name = format!("historical metadata with {needle}");
    f.runtime()
        .store
        .put_object(&key, serde_json::to_vec(&metadata_changed).unwrap())
        .unwrap();
    assert_eq!(metadata_changed.archived_at, old_clock);
    assert_eq!(metadata_changed.transcript, old_transcript);
    let metadata_job = f.enqueue(&f.owner, target.id, false).await;
    f.run_next().await;
    let mut media_changed = f
        .runtime()
        .store
        .get_session_manifest(original.user_id, original.session_id)
        .unwrap()
        .unwrap();
    assert!(!media_changed.session_name.contains(needle));
    assert!(f.receipt(&f.owner, metadata_job.id).await.objects_rewritten > 0);
    assert_eq!(
        serde_json::to_value(f.lines(&original)).unwrap(),
        serde_json::to_value(&original_lines).unwrap()
    );
    let old_clock = media_changed.archived_at;
    let old_transcript = media_changed.transcript.clone();
    media_changed.media = Some(vec![archive_format::MediaEntry {
        media_id,
        kind: meta.kind,
        content_type: meta.content_type,
        bytes: meta.bytes,
        object_key: archive_format::media_key(original.user_id, original.session_id, media_id),
        uploaded_at: meta.uploaded_at,
    }]);
    f.runtime()
        .store
        .put_object(&key, serde_json::to_vec(&media_changed).unwrap())
        .unwrap();
    assert_eq!(media_changed.archived_at, old_clock);
    assert_eq!(media_changed.transcript, old_transcript);
    let media_job = f.enqueue(&f.owner, target.id, false).await;
    f.run_next().await;
    assert!(f
        .runtime()
        .store
        .get_media_bytes(original.user_id, original.session_id, media_id)
        .unwrap()
        .is_none());
    assert!(media_removed(&mut f.state.conn().unwrap(), media_id).unwrap());
    assert!(f
        .runtime()
        .store
        .get_session_manifest(original.user_id, original.session_id)
        .unwrap()
        .unwrap()
        .media
        .unwrap()
        .is_empty());
    assert!(f.receipt(&f.owner, media_job.id).await.objects_rewritten > 0);
    assert_eq!(
        serde_json::to_value(f.lines(&original)).unwrap(),
        serde_json::to_value(original_lines).unwrap()
    );
    let stable_manifest = f.runtime().store.get_object(&key).unwrap().unwrap();
    let stable_transcript = f.archive_bytes(&original);
    let unchanged = f.enqueue(&f.owner, target.id, false).await;
    f.run_next().await;
    assert_eq!(f.receipt(&f.owner, unchanged.id).await.objects_rewritten, 0);
    assert_eq!(
        f.runtime().store.get_object(&key).unwrap().unwrap(),
        stable_manifest
    );
    assert_eq!(f.archive_bytes(&original), stable_transcript);
    // Persisted fingerprints must not become a reusable bare-plaintext hash.
    // A fresh runtime has a new private cache key, so it must revalidate and
    // replace coverage even though the archived objects need no rewrite.
    use crate::schema::secret_coverage as coverage;
    let coverage_key = format!(
        "secret/{}/{}",
        target.id,
        archive_format::transcript_key(original.user_id, original.session_id)
    );
    let previous_fingerprint = coverage::table
        .find(&coverage_key)
        .select(coverage::object_generation)
        .first::<String>(&mut f.state.conn().unwrap())
        .unwrap();
    let mut restarted = f.state.as_ref().clone();
    let runtime =
        Arc::new(crate::archive::ArchiveRuntime::new(f.runtime().config.clone()).unwrap());
    *runtime.media_db_pool.write().unwrap() = Some(f.state.db_pool.clone());
    restarted.archive = Some(runtime);
    let cold = f.enqueue(&f.owner, target.id, false).await;
    tokio::task::spawn_blocking(move || super::run_next(&restarted))
        .await
        .unwrap()
        .unwrap();
    let fresh_fingerprint = coverage::table
        .find(&coverage_key)
        .select(coverage::object_generation)
        .first::<String>(&mut f.state.conn().unwrap())
        .unwrap();
    assert_ne!(previous_fingerprint, fresh_fingerprint);
    assert_eq!(f.receipt(&f.owner, cold.id).await.objects_rewritten, 0);
    assert_eq!(
        f.runtime().store.get_object(&key).unwrap().unwrap(),
        stable_manifest
    );
    assert_eq!(f.archive_bytes(&original), stable_transcript);
}

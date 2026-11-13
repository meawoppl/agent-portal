//! Write-only, owner-approved batches. Transcript rows contain only proposal IDs.
use super::*;
use crate::schema::{secret_deposit_proposals as proposals, sessions};
use crate::secrets::crypto::Envelope;

#[derive(Queryable, Selectable)]
#[diesel(table_name = proposals)]
struct Proposal {
    id: Uuid,
    owner_user_id: Uuid,
    session_id: Uuid,
    launcher_id: Uuid,
    proxy_token_id: Uuid,
    organization_id: Uuid,
    state: String,
    metadata: serde_json::Value,
    envelopes: Option<serde_json::Value>,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
}
impl Proposal {
    fn info(&self) -> Result<SecretDepositInfo, AppError> {
        let _ = self.created_at;
        Ok(SecretDepositInfo {
            id: self.id,
            session_id: self.session_id,
            state: self.state.clone(),
            expires_at: self.expires_at.to_rfc3339(),
            items: serde_json::from_value(self.metadata.clone())
                .map_err(|_| AppError::Internal("Invalid proposal metadata".into()))?,
        })
    }
    fn same_process(&self, p: &bindings::ProcessAudience) -> bool {
        (
            self.owner_user_id,
            self.session_id,
            self.launcher_id,
            self.proxy_token_id,
        ) == (p.user, p.session, p.launcher, p.token)
    }
}
fn load(conn: &mut PgConnection, id: Uuid) -> Result<Proposal, AppError> {
    proposals::table
        .find(id)
        .for_update()
        .select(Proposal::as_select())
        .first(conn)
        .map_err(|_| AppError::NotFound("Secret proposal not found"))
}
pub(crate) fn expire(conn: &mut PgConnection) -> Result<(), AppError> {
    diesel::update(
        proposals::table
            .filter(proposals::state.eq("pending"))
            .filter(proposals::expires_at.le(Utc::now())),
    )
    .set((
        proposals::state.eq("expired"),
        proposals::envelopes.eq(None::<serde_json::Value>),
    ))
    .execute(conn)?;
    Ok(())
}
fn check_owner(conn: &mut PgConnection, row: &Proposal, user: Uuid) -> Result<(), AppError> {
    store::active_user(conn, user)?;
    if row.owner_user_id != user
        || sessions::table
            .find(row.session_id)
            .select(sessions::user_id)
            .first::<Uuid>(conn)?
            != user
    {
        return Err(AppError::Forbidden);
    }
    Ok(())
}
fn current(state: &AppState, conn: &mut PgConnection, row: &Proposal) -> Result<(), AppError> {
    check_owner(conn, row, row.owner_user_id)?;
    let session = sessions::table
        .find(row.session_id)
        .first::<crate::models::Session>(conn)?;
    if bindings::current_process(conn, row.owner_user_id, row.session_id)? != row.proxy_token_id
        || session.launcher_id != Some(row.launcher_id)
        || state.session_manager.launcher_owner(row.launcher_id) != Some(row.owner_user_id)
        || !state
            .session_manager
            .is_proxy_connected(&session.session_key)
    {
        return Err(AppError::Conflict(
            "Requesting agent is no longer connected; submit a new proposal",
        ));
    }
    Ok(())
}
// Fixed diagnostics and bounded slow-client buffering, before acquiring a DB connection.
struct DepositBody(SecretDepositCreate);
impl axum::extract::FromRequest<Arc<AppState>> for DepositBody {
    type Rejection = AppError;
    async fn from_request(
        request: axum::extract::Request,
        state: &Arc<AppState>,
    ) -> Result<Self, AppError> {
        let _permit = state
            .secrets
            .body_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::ServiceUnavailable("Secret request capacity reached"))?;
        let bytes = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            axum::body::to_bytes(request.into_body(), 400_000),
        )
        .await
        .map_err(|_| AppError::GatewayTimeout("Secret proposal timed out"))?
        .map_err(|_| AppError::PayloadTooLarge("Secret proposal exceeds body limit".into()))?;
        serde_json::from_slice(&bytes)
            .map(Self)
            .map_err(|_| AppError::BadRequest("Invalid secret proposal"))
    }
}
async fn create(
    State(state): State<Arc<AppState>>,
    _process: access_requests::BoundProcess,
    headers: HeaderMap,
    DepositBody(request): DepositBody,
) -> Result<Json<SecretDepositInfo>, AppError> {
    if request.items.is_empty() || request.items.len() > 16 {
        return Err(AppError::BadRequest("Propose between 1 and 16 secrets"));
    }
    let _permit = state
        .secrets
        .mutation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::ServiceUnavailable("Another secrets mutation is in progress; retry")
        })?;
    let (process, organization) = {
        let mut conn = state.conn()?;
        let process = bindings::authenticated_process(&state, &mut conn, &headers)?;
        // An ID identifies an immutable submission. Replays never replace its values.
        if let Some(row) = proposals::table
            .find(request.request_id)
            .select(Proposal::as_select())
            .first::<Proposal>(&mut conn)
            .optional()?
        {
            if !row.same_process(&process) {
                return Err(AppError::Forbidden);
            }
            expire(&mut conn)?;
            return Ok(Json(load(&mut conn, request.request_id)?.info()?));
        }
        let org = secrets_generation::table
            .find(1i16)
            .select(secrets_generation::organization_id)
            .first::<Uuid>(&mut conn)?;
        (process, org)
    };
    let mut items = Vec::new();
    let mut envelopes = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut total = 0usize;
    for input in request.items {
        validated_name(&input.name, &input.description)?;
        if !names.insert((
            format!("{:?}", input.namespace),
            input.name.to_ascii_lowercase(),
        )) || input.share.len() > 8
            || (input.namespace == SecretNamespace::Organization && !input.share.is_empty())
        {
            return Err(AppError::BadRequest(
                "Duplicate name or invalid sharing audience",
            ));
        }
        let value = checked_value(input.value)?;
        total += value.bytes().len();
        if total > 256 * 1024 {
            return Err(AppError::PayloadTooLarge(
                "Batch values exceed 256 KiB".into(),
            ));
        }
        let mut recipients = Vec::new();
        let existing;
        {
            let mut conn = state.conn()?;
            if input.namespace == SecretNamespace::Organization
                && !store::curator(&mut conn, process.user)?
            {
                return Err(AppError::Forbidden);
            }
            let namespace = if input.namespace == SecretNamespace::Personal {
                "personal"
            } else {
                "organization"
            };
            let query = secrets::table
                .filter(secrets::organization_id.eq(organization))
                .filter(secrets::namespace.eq(namespace))
                .filter(secrets::name.eq(&input.name));
            existing = if input.namespace == SecretNamespace::Personal {
                query
                    .filter(secrets::owner_user_id.eq(process.user))
                    .first::<SecretRow>(&mut conn)
                    .optional()?
            } else {
                query.first::<SecretRow>(&mut conn).optional()?
            };
            if let Some(row) = &existing {
                store::require_live(row)?;
                if !store::rights(&mut conn, row, process.user)?.write {
                    return Err(AppError::Forbidden);
                }
            }
            for selector in input.share {
                let recipient =
                    members::deposit_recipient(&state, &mut conn, process.user, &selector)?;
                if !recipients
                    .iter()
                    .any(|r: &SecretDepositRecipient| r.user_id == recipient.user_id)
                {
                    recipients.push(recipient);
                }
            }
        }
        let update = if let Some(row) = &existing {
            let version_id = row
                .current_version_id
                .ok_or(AppError::Conflict("Secret has no current version"))?;
            let version = secret_versions::table
                .find(version_id)
                .select(secret_versions::ordinal)
                .first::<i32>(&mut *state.conn()?)?;
            Some(SecretDepositUpdate {
                version_id,
                version,
                generation: row.acl_generation,
            })
        } else {
            None
        };
        let description = if input.description.is_empty() {
            existing
                .as_ref()
                .map(|row| row.description.clone())
                .unwrap_or_default()
        } else {
            input.description
        };
        let item = SecretDepositItem {
            update,
            secret_id: existing
                .as_ref()
                .map(|row| row.id)
                .unwrap_or_else(Uuid::new_v4),
            version_id: Uuid::new_v4(),
            namespace: input.namespace,
            name: input.name,
            description,
            recipients,
        };
        let envelope = state
            .secrets
            .crypto()?
            .seal(
                Identity {
                    organization,
                    secret: item.secret_id,
                    version: item.version_id,
                },
                &value,
            )
            .await
            .map_err(crypto_error)?;
        items.push(item);
        envelopes.push(envelope);
    }
    let (info, message) = {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            diesel::sql_query("SELECT pg_advisory_xact_lock(1936024434,3)").execute(conn)?;
            expire(conn)?;
            // Recheck after encryption and across other backend instances.
            let latest = bindings::authenticated_process(&state, conn, &headers)?;
            if (latest.token, latest.session, latest.launcher)
                != (process.token, process.session, process.launcher)
            {
                return Err(AppError::Forbidden);
            }
            if let Some(row) = proposals::table
                .find(request.request_id)
                .select(Proposal::as_select())
                .first::<Proposal>(conn)
                .optional()?
            {
                if !row.same_process(&process) {
                    return Err(AppError::Forbidden);
                }
                return Ok((row.info()?, None));
            }
            let pending = proposals::table.filter(proposals::state.eq("pending"));
            if pending.count().get_result::<i64>(conn)? >= 128
                || pending
                    .filter(proposals::owner_user_id.eq(process.user))
                    .count()
                    .get_result::<i64>(conn)?
                    >= 4
                || proposals::table
                    .filter(proposals::owner_user_id.eq(process.user))
                    .filter(proposals::created_at.gt(Utc::now() - chrono::Duration::hours(1)))
                    .count()
                    .get_result::<i64>(conn)?
                    >= 30
            {
                return Err(AppError::ServiceUnavailable(
                    "Secret proposal rate limit reached",
                ));
            }
            diesel::insert_into(proposals::table)
                .values((
                    proposals::id.eq(request.request_id),
                    proposals::owner_user_id.eq(process.user),
                    proposals::session_id.eq(process.session),
                    proposals::launcher_id.eq(process.launcher),
                    proposals::proxy_token_id.eq(process.token),
                    proposals::organization_id.eq(organization),
                    proposals::metadata.eq(encode(&items)?),
                    proposals::envelopes.eq(Some(encode(&envelopes)?)),
                ))
                .execute(conn)?;
            let row = load(conn, request.request_id)?;
            current(&state, conn, &row)?;
            let session = sessions::table
                .find(process.session)
                .first::<crate::models::Session>(conn)?;
            let portal = shared::PortalMessage::with_content(vec![
                shared::PortalContent::SecretDepositProposal {
                    request_id: request.request_id,
                },
            ]);
            let message = crate::secrets::redaction::insert_message(
                conn,
                crate::models::NewMessage {
                    session_id: process.session,
                    role: shared::MessageRole::Portal.to_string(),
                    content: portal.to_json().to_string(),
                    user_id: process.user,
                    agent_type: session.agent_type,
                    provenance_kind: None,
                    provenance_session_id: None,
                    provenance_agent_type: None,
                },
            )?;
            store::audit_context(
                conn,
                process.user,
                None,
                "deposit_proposed",
                "pending",
                crate::audit::Context {
                    session: Some(process.session),
                    launcher: Some(process.launcher),
                    operation: Some(row.id),
                    agent: true,
                },
            )?;
            Ok((row.info()?, Some(message)))
        })?
    };
    access_requests::publish(&state, message)?;
    Ok(Json(info))
}
async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretDepositInfo>, AppError> {
    let browser = if headers.contains_key(header::AUTHORIZATION) {
        None
    } else {
        Some(principal(&state, &headers, &cookies, false)?.user)
    };
    let mut conn = state.conn()?;
    let info = conn.transaction::<_, AppError, _>(|conn| {
        expire(conn)?;
        let row = load(conn, id)?;
        if headers.contains_key(header::AUTHORIZATION) {
            if !row.same_process(&bindings::authenticated_process(&state, conn, &headers)?) {
                return Err(AppError::Forbidden);
            }
        } else {
            check_owner(conn, &row, browser.ok_or(AppError::Forbidden)?)?;
        }
        row.info()
    })?;
    Ok(Json(info))
}
fn edited(
    mut items: Vec<SecretDepositItem>,
    edits: Option<Vec<SecretDepositEdit>>,
) -> Result<Vec<SecretDepositItem>, AppError> {
    if let Some(edits) = edits {
        if edits.len() != items.len() {
            return Err(AppError::BadRequest("Review the entire proposal batch"));
        }
        for (item, edit) in items.iter_mut().zip(edits) {
            validated_name(&edit.name, &edit.description)?;
            if edit
                .recipients
                .iter()
                .any(|id| !item.recipients.iter().any(|r| r.user_id == *id))
            {
                return Err(AppError::BadRequest("Approval cannot add new recipients"));
            }
            if item.update.is_some() && item.name != edit.name {
                return Err(AppError::BadRequest(
                    "Update targets cannot be renamed during approval",
                ));
            }
            item.name = edit.name;
            item.description = edit.description;
            item.recipients
                .retain(|r| edit.recipients.contains(&r.user_id));
        }
    }
    Ok(items)
}
fn insert_approved_item(
    conn: &mut PgConnection,
    organization: Uuid,
    user: Uuid,
    item: &SecretDepositItem,
    envelope: &Envelope,
    context: crate::audit::Context,
) -> Result<(), AppError> {
    if item.namespace == SecretNamespace::Organization && !store::curator(conn, user)? {
        return Err(AppError::Forbidden);
    }
    let namespace = if item.namespace == SecretNamespace::Personal {
        "personal"
    } else {
        "organization"
    };
    let (ordinal, acl_generation) = if let Some(expected) = &item.update {
        let row = lock(conn, item.secret_id)?;
        store::require_live(&row)?;
        store::require_generation(&row, expected.generation)?;
        if row.organization_id != organization
            || row.namespace != namespace
            || row.name != item.name
            || (item.namespace == SecretNamespace::Personal && row.owner_user_id != user)
            || row.current_version_id != Some(expected.version_id)
        {
            return Err(AppError::Conflict(
                "Secret changed since review; submit a new set request",
            ));
        }
        if !store::rights(conn, &row, user)?.write {
            return Err(AppError::Forbidden);
        }
        let version = secret_versions::table
            .find(expected.version_id)
            .select(secret_versions::ordinal)
            .first::<i32>(conn)?;
        if version != expected.version {
            return Err(AppError::Conflict("Secret version changed"));
        }
        (
            version
                .checked_add(1)
                .ok_or(AppError::Conflict("Version limit"))?,
            row.acl_generation + 1,
        )
    } else {
        diesel::insert_into(secrets::table)
            .values((
                secrets::id.eq(item.secret_id),
                secrets::organization_id.eq(organization),
                secrets::owner_user_id.eq(user),
                secrets::namespace.eq(namespace),
                secrets::name.eq(&item.name),
                secrets::description.eq(&item.description),
                secrets::current_version_id.eq(Some(item.version_id)),
            ))
            .execute(conn)
            .map_err(|_| {
                AppError::Conflict("A new secret name is now unavailable; submit a new set request")
            })?;
        (1, 1)
    };
    version_quota(conn, envelope.ciphertext.len())?;
    insert_version(
        conn,
        Identity {
            organization,
            secret: item.secret_id,
            version: item.version_id,
        },
        ordinal,
        user,
        envelope,
    )?;
    if item.update.is_some() {
        diesel::update(secrets::table.find(item.secret_id))
            .set((
                secrets::current_version_id.eq(Some(item.version_id)),
                secrets::description.eq(&item.description),
                secrets::acl_generation.eq(acl_generation),
                secrets::updated_at.eq(Utc::now()),
            ))
            .execute(conn)?;
    }
    for recipient in &item.recipients {
        store::active_user(conn, recipient.user_id)?;
        if let Some(grant) = secret_grants::table
            .find((item.secret_id, recipient.user_id))
            .first::<store::GrantRow>(conn)
            .optional()?
        {
            if !grant.can_read
                || !grant.can_use
                || grant.expires_at.is_some_and(|time| time <= Utc::now())
            {
                return Err(AppError::Conflict(
                    "Use Settings to change an existing recipient grant",
                ));
            }
            continue; // Preserve existing rights and expiration exactly.
        }
        diesel::insert_into(secret_grants::table)
            .values((
                secret_grants::secret_id.eq(item.secret_id),
                secret_grants::grantee_user_id.eq(recipient.user_id),
                secret_grants::can_read.eq(true),
                secret_grants::can_use.eq(true),
                secret_grants::can_write.eq(false),
                secret_grants::can_manage.eq(false),
                secret_grants::generation.eq(acl_generation),
            ))
            .execute(conn)?;
        store::record_acl_change(
            conn,
            item.secret_id,
            user,
            recipient.user_id,
            store::Rights {
                read: true,
                use_value: true,
                write: false,
                manage: false,
            },
            acl_generation,
            "grant",
        )?;
    }
    generation(conn, item.secret_id, "version_created")?;
    store::audit_context(
        conn,
        user,
        Some(item.secret_id),
        if item.update.is_some() {
            "deposit_updated"
        } else {
            "deposit_created"
        },
        "allowed",
        context,
    )
}

async fn decision(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
    SecretJson(request): SecretJson<SecretDepositDecision>,
) -> Result<Json<SecretDepositInfo>, AppError> {
    auth::csrf(&state, &headers)?;
    if request.approve {
        auth::require_verification(&cookies, &state, user)?;
    }
    let _permit = state
        .secrets
        .mutation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::ServiceUnavailable("Another secrets mutation is in progress; retry")
        })?;
    // Authenticate staged ciphertext with the current key before committing
    // anything to the live redaction registry. Values are immediately zeroized.
    if request.approve {
        let row = {
            let mut conn = state.conn()?;
            let row = load(&mut conn, id)?;
            check_owner(&mut conn, &row, user)?;
            row
        };
        if row.state == "pending" {
            let items = row.info()?.items;
            let envelopes: Vec<Envelope> = serde_json::from_value(
                row.envelopes
                    .ok_or(AppError::Conflict("Proposal values unavailable"))?,
            )
            .map_err(|_| AppError::Internal("Invalid proposal envelopes".into()))?;
            if items.len() != envelopes.len() {
                return Err(AppError::Conflict("Proposal is incomplete"));
            }
            for (item, envelope) in items.iter().zip(&envelopes) {
                let _value = state
                    .secrets
                    .crypto()?
                    .open(
                        Identity {
                            organization: row.organization_id,
                            secret: item.secret_id,
                            version: item.version_id,
                        },
                        envelope,
                    )
                    .await
                    .map_err(crypto_error)?;
            }
        }
    }
    let info = {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            expire(conn)?;
            let row = load(conn, id)?;
            check_owner(conn, &row, user)?;
            if row.state != "pending" {
                return row.info();
            }
            let items = if request.approve {
                edited(row.info()?.items, request.items)?
            } else {
                row.info()?.items
            };
            if request.approve {
                let updates: std::collections::BTreeSet<_> = items
                    .iter()
                    .filter(|item| item.update.is_some())
                    .map(|item| item.secret_id)
                    .collect();
                let confirmed: std::collections::BTreeSet<_> =
                    request.confirm_updates.into_iter().collect();
                if updates != confirmed {
                    return Err(AppError::BadRequest(
                        "Review and explicitly confirm every secret update",
                    ));
                }
                current(&state, conn, &row)?;
                let envelopes: Vec<Envelope> = serde_json::from_value(
                    row.envelopes
                        .ok_or(AppError::Conflict("Proposal values unavailable"))?,
                )
                .map_err(|_| AppError::Internal("Invalid proposal envelopes".into()))?;
                if envelopes.len() != items.len() {
                    return Err(AppError::Conflict("Proposal is incomplete"));
                }
                diesel::sql_query("SELECT pg_advisory_xact_lock(1936024434,1)").execute(conn)?;
                let grants: usize = items.iter().map(|i| i.recipients.len()).sum();
                if secret_grants::table.count().get_result::<i64>(conn)? + grants as i64 > 8192 {
                    return Err(AppError::Conflict("Sharing quota reached"));
                }
                for (item, envelope) in items.iter().zip(envelopes) {
                    insert_approved_item(
                        conn,
                        row.organization_id,
                        user,
                        item,
                        &envelope,
                        crate::audit::Context {
                            session: Some(row.session_id),
                            launcher: Some(row.launcher_id),
                            operation: Some(id),
                            agent: false,
                        },
                    )?;
                }
                // Check again at commit boundary; expired batches never commit.
                if row.expires_at <= Utc::now() {
                    return Err(AppError::Conflict("Proposal expired"));
                }
            }
            let result = if request.approve {
                "approved"
            } else {
                "denied"
            };
            diesel::update(proposals::table.find(id))
                .set((
                    proposals::state.eq(result),
                    proposals::metadata.eq(encode(&items)?),
                    proposals::envelopes.eq(None::<serde_json::Value>),
                ))
                .execute(conn)?;
            store::audit_context(
                conn,
                user,
                None,
                "deposit_decision",
                result,
                crate::audit::Context {
                    session: Some(row.session_id),
                    launcher: Some(row.launcher_id),
                    operation: Some(id),
                    agent: false,
                },
            )?;
            load(conn, id)?.info()
        })?
    };
    if info.state == "approved" {
        state.secrets.refresh_redaction(&state.db_pool).await?;
    }
    Ok(Json(info))
}
pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/secrets/deposits", post(create))
        .route("/api/secrets/deposits/{id}", get(status))
        .route("/api/secrets/deposits/{id}/decision", post(decision))
}

fn encode(value: &impl serde::Serialize) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value)
        .map_err(|_| AppError::Internal("Could not encode secret proposal".into()))
}
pub(crate) async fn sweep(state: Arc<AppState>) {
    let result = tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        expire(&mut *state.conn()?)
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        tracing::warn!(
            "{} Proposal expiry cleanup will retry",
            crate::markers::SECRET_PROPOSAL_EXPIRY_FAILED
        );
    }
}

#[cfg(test)]
#[path = "deposit_tests.rs"]
mod tests;

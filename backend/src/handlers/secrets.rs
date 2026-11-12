//! Browser responses contain metadata only. Dedicated credentials are checked
//! here AND by the service policy before any saved value is released.
use crate::{
    errors::AppError,
    schema::{secret_grants, secret_versions, secrets, secrets_generation},
    secrets::{
        auth::{self, Principal},
        crypto::{Identity, SensitiveValue},
        store::{self, SecretRow, VersionRow},
    },
    AppState,
};
use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State},
    http::{header, request::Parts, HeaderMap},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use diesel::prelude::*;
use shared::api::*;
use std::sync::Arc;
use uuid::Uuid;
mod access_requests;
mod bindings;
mod enrollment;
mod idempotency;
mod members;
const MAX_RULE_SOURCE_BYTES: i64 = 1024 * 1024;
/// Json rejection details can contain input scalar values. Secret routes use
/// a fixed error instead of reflecting parser diagnostics or request bodies.
pub struct SecretJson<T>(pub T);
impl<S, T> axum::extract::FromRequest<S> for SecretJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = AppError;
    async fn from_request(request: axum::extract::Request, state: &S) -> Result<Self, AppError> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|_| AppError::BadRequest("Invalid secrets request body"))
    }
}
struct SecretPrincipal(Principal);
impl FromRequestParts<Arc<AppState>> for SecretPrincipal {
    type Rejection = Response;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        match auth::credential(state, &parts.headers).map_err(IntoResponse::into_response)? {
            Some(principal) => Ok(Self(principal)),
            None => {
                let user = crate::auth::BrowserUserId::from_request_parts(parts, state).await?;
                Ok(Self(Principal {
                    user: user.0,
                    browser: true,
                    metadata: true,
                    mutation: true,
                    read: false,
                }))
            }
        }
    }
}
pub fn principal(
    state: &AppState,
    headers: &HeaderMap,
    cookies: &tower_cookies::Cookies,
    is_mutation: bool,
) -> Result<Principal, AppError> {
    let principal = if let Some(principal) = auth::credential(state, headers)? {
        principal
    } else {
        let cookie = cookies
            .signed(&state.cookie_key)
            .get(shared::protocol::SESSION_COOKIE_NAME)
            .ok_or(AppError::Unauthorized)?;
        let user = Uuid::parse_str(cookie.value()).map_err(|_| AppError::Unauthorized)?;
        store::active_user(&mut *state.conn()?, user)?;
        Principal {
            user,
            browser: true,
            metadata: true,
            mutation: true,
            read: false,
        }
    };
    if let Some(expected) = headers.get("X-Portal-Account") {
        if expected.to_str().ok().and_then(|v| Uuid::parse_str(v).ok()) != Some(principal.user) {
            return Err(AppError::Forbidden);
        }
    }
    if is_mutation {
        mutation(state, headers, principal)?
    };
    Ok(principal)
}
fn mutation(state: &AppState, headers: &HeaderMap, principal: Principal) -> Result<(), AppError> {
    if !principal.mutation {
        return Err(AppError::Forbidden);
    };
    if principal.browser {
        auth::csrf(state, headers)?
    };
    Ok(())
}
fn crypto_error(_: crate::secrets::crypto::CryptoError) -> AppError {
    AppError::ServiceUnavailable(
        "Secret key provider unavailable or ciphertext authentication failed",
    )
}
fn checked_value(mut value: SecretValue) -> Result<SensitiveValue, AppError> {
    let sensitive = SensitiveValue::new(std::mem::take(&mut value.0).into_bytes());
    if sensitive.bytes().is_empty() || sensitive.bytes().len() > 65536 {
        return Err(AppError::BadRequest(
            "Secret values must contain 1 to 65536 UTF-8 bytes",
        ));
    };
    Ok(sensitive)
}
fn validated_name(name: &str, description: &str) -> Result<(), AppError> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_- .".contains(&c))
        || description.len() > 2048
    {
        return Err(AppError::BadRequest("Invalid secret name or description"));
    };
    Ok(())
}
fn lock(conn: &mut PgConnection, id: Uuid) -> Result<SecretRow, AppError> {
    secrets::table
        .find(id)
        .for_update()
        .first(conn)
        .map_err(|_| AppError::NotFound("Secret not found"))
}
fn generation(conn: &mut PgConnection, id: Uuid, kind: &str) -> Result<(), AppError> {
    use crate::schema::secret_outbox as outbox;
    let value = diesel::update(secrets_generation::table.find(1i16))
        .set(secrets_generation::generation.eq(secrets_generation::generation + 1))
        .returning(secrets_generation::generation)
        .get_result::<i64>(conn)?;
    diesel::insert_into(outbox::table)
        .values((
            outbox::id.eq(Uuid::new_v4()),
            outbox::kind.eq(kind),
            outbox::target_id.eq(id),
            outbox::generation.eq(value),
        ))
        .execute(conn)?;
    Ok(())
}
/// Retained ciphertext length equals plaintext length for GCM. Bound all
/// registered values to 1 MiB so raw + base64 + worst-case URL forms fit the
/// redactor's 16 MiB budget. Serializing on the generation row closes races.
fn version_quota(conn: &mut PgConnection, bytes: usize) -> Result<(), AppError> {
    secrets_generation::table
        .find(1i16)
        .for_update()
        .select(secrets_generation::generation)
        .first::<i64>(conn)?;
    #[derive(QueryableByName)]
    struct Total {
        #[diesel(sql_type=diesel::sql_types::BigInt)]
        bytes: i64,
    }
    let count = secret_versions::table.count().get_result::<i64>(conn)?;
    if count >= 4096 {
        return Err(AppError::Conflict(
            "Organization retained version quota reached (4096)",
        ));
    };
    let current = diesel::sql_query(
        "SELECT ((SELECT COALESCE(SUM(octet_length(encrypted_value)),0) FROM secret_versions)
          + (SELECT COALESCE(SUM(octet_length(v.encrypted_value)),0)
             FROM secret_occurrences o JOIN secret_versions v ON v.id=o.rule_version_id
             WHERE o.record_kind='reader'))::bigint AS bytes",
    )
    .get_result::<Total>(conn)?
    .bytes;
    if current.saturating_add(bytes as i64) > MAX_RULE_SOURCE_BYTES {
        return Err(AppError::Conflict(
            "Organization vault and consented reader quota reached (1 MiB)",
        ));
    };
    Ok(())
}
fn insert_version(
    conn: &mut PgConnection,
    id: Identity,
    ordinal: i32,
    actor: Uuid,
    envelope: &crate::secrets::crypto::Envelope,
) -> Result<(), AppError> {
    diesel::insert_into(secret_versions::table)
        .values((
            secret_versions::id.eq(id.version),
            secret_versions::secret_id.eq(id.secret),
            secret_versions::ordinal.eq(ordinal),
            secret_versions::encrypted_value.eq(&envelope.ciphertext),
            secret_versions::nonce.eq(&envelope.nonce),
            secret_versions::auth_tag.eq(&envelope.tag),
            secret_versions::wrapped_dek.eq(&envelope.wrapped_dek),
            secret_versions::wrapping_key_id.eq(&envelope.wrapping_key_id),
            secret_versions::crypto_format.eq(envelope.crypto_format),
            secret_versions::created_by.eq(actor),
        ))
        .execute(conn)?;
    Ok(())
}
async fn create(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    SecretJson(request): SecretJson<SecretCreateRequest>,
) -> Result<Json<SecretMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    validated_name(&request.name, &request.description)?;
    let value = checked_value(request.value)?;
    let organization = {
        let mut conn = state.conn()?;
        if request.namespace == SecretNamespace::Organization
            && !store::curator(&mut conn, principal.user)?
        {
            return Err(AppError::Forbidden);
        };
        store::active_user(&mut conn, principal.user)?;
        secrets_generation::table
            .find(1i16)
            .select(secrets_generation::organization_id)
            .first(&mut conn)?
    };
    let id = Identity {
        organization,
        secret: Uuid::new_v5(&principal.user, idempotency::key(&headers)?.as_bytes()),
        version: Uuid::new_v4(),
    };
    // A process may crash after commit but before writing the transport receipt.
    // The actor/key-derived ID makes a retried create return its existing key.
    let previous = {
        let mut conn = state.conn()?;
        secrets::table
            .find(id.secret)
            .first::<SecretRow>(&mut conn)
            .optional()?
    };
    if let Some(previous) = previous {
        state.secrets.refresh_redaction(&state.db_pool).await?;
        return Ok(Json(store::metadata(
            &mut *state.conn()?,
            &previous,
            principal.user,
        )?));
    }
    let envelope = state
        .secrets
        .crypto()?
        .seal(id, &value)
        .await
        .map_err(crypto_error)?;
    {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            store::active_user(conn, principal.user)?;
            if request.namespace == SecretNamespace::Organization
                && !store::curator(conn, principal.user)?
            {
                return Err(AppError::Forbidden);
            };
            diesel::insert_into(secrets::table)
                .values((
                    secrets::id.eq(id.secret),
                    secrets::organization_id.eq(organization),
                    secrets::owner_user_id.eq(principal.user),
                    secrets::namespace.eq(if request.namespace == SecretNamespace::Personal {
                        "personal"
                    } else {
                        "organization"
                    }),
                    secrets::name.eq(&request.name),
                    secrets::description.eq(&request.description),
                    secrets::current_version_id.eq(Some(id.version)),
                ))
                .execute(conn)
                .map_err(|_| AppError::Conflict("Secret name is reserved or unavailable"))?;
            version_quota(conn, envelope.ciphertext.len())?;
            insert_version(conn, id, 1, principal.user, &envelope)?;
            generation(conn, id.secret, "version_created")?;
            store::audit(conn, principal.user, Some(id.secret), "create", "allowed")?;
            Ok(())
        })?;
    }
    state.secrets.refresh_redaction(&state.db_pool).await?;
    let mut conn = state.conn()?;
    let row = store::load(&mut conn, id.secret)?;
    Ok(Json(store::metadata(&mut conn, &row, principal.user)?))
}
async fn list(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    Query(query): Query<SecretListQuery>,
) -> Result<Json<SecretListResponse>, AppError> {
    if !principal.metadata {
        return Err(AppError::Forbidden);
    };
    let mut conn = state.conn()?;
    store::active_user(&mut conn, principal.user)?;
    let rows = secrets::table
        .order((secrets::namespace, secrets::name))
        .load::<SecretRow>(&mut conn)?;
    let mut result = vec![];
    for row in rows {
        if row.deleted_at.is_some() != query.deleted {
            continue;
        };
        if let Some(scope) = query.scope {
            if match scope {
                SecretScope::Personal => {
                    row.namespace != "personal" || row.owner_user_id != principal.user
                }
                SecretScope::Shared => {
                    row.namespace != "personal" || row.owner_user_id == principal.user
                }
                SecretScope::Organization => row.namespace != "organization",
            } {
                continue;
            }
        };
        if store::rights(&mut conn, &row, principal.user)?.any() {
            result.push(store::metadata(&mut conn, &row, principal.user)?);
        }
    }
    Ok(Json(SecretListResponse {
        secrets: result,
        can_manage_organization: store::curator(&mut conn, principal.user)?,
    }))
}
async fn info(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretMetadata>, AppError> {
    if !principal.metadata {
        return Err(AppError::Forbidden);
    };
    let mut conn = state.conn()?;
    let row = store::load(&mut conn, id)?;
    Ok(Json(store::metadata(&mut conn, &row, principal.user)?))
}
async fn resolve(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    SecretJson(request): SecretJson<SecretResolveRequest>,
) -> Result<Json<SecretMetadata>, AppError> {
    if !principal.metadata {
        return Err(AppError::Forbidden);
    };
    let mut conn = state.conn()?;
    let (namespace, name) = request
        .reference
        .split_once('/')
        .ok_or(AppError::BadRequest(
            "Use personal/NAME, org/NAME or id/UUID",
        ))?;
    let row = match namespace {
        "id" => store::load(
            &mut conn,
            Uuid::parse_str(name).map_err(|_| AppError::BadRequest("Invalid secret UUID"))?,
        )?,
        "personal" => secrets::table
            .filter(secrets::namespace.eq("personal"))
            .filter(secrets::owner_user_id.eq(principal.user))
            .filter(secrets::name.eq(name))
            .first::<SecretRow>(&mut conn)
            .map_err(|_| AppError::NotFound("Secret not found"))?,
        "org" => secrets::table
            .filter(secrets::namespace.eq("organization"))
            .filter(secrets::name.eq(name))
            .first::<SecretRow>(&mut conn)
            .map_err(|_| AppError::NotFound("Secret not found"))?,
        _ => {
            return Err(AppError::BadRequest(
                "Use personal/NAME, org/NAME or id/UUID",
            ))
        }
    };
    Ok(Json(store::metadata(&mut conn, &row, principal.user)?))
}
async fn versions(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretVersionsResponse>, AppError> {
    if !principal.metadata {
        return Err(AppError::Forbidden);
    };
    let mut conn = state.conn()?;
    let row = store::load(&mut conn, id)?;
    store::metadata(&mut conn, &row, principal.user)?;
    let versions = secret_versions::table
        .filter(secret_versions::secret_id.eq(id))
        .order(secret_versions::ordinal.desc())
        .load::<VersionRow>(&mut conn)?
        .into_iter()
        .map(|v| SecretVersionMetadata {
            version: v.ordinal,
            created_at: v.created_at.to_rfc3339(),
            created_by: v.created_by,
            current: row.current_version_id == Some(v.id),
        })
        .collect();
    Ok(Json(SecretVersionsResponse { versions }))
}
async fn write(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    SecretJson(request): SecretJson<SecretWriteRequest>,
) -> Result<Json<SecretMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    let value = checked_value(request.value)?;
    let row = {
        let mut conn = state.conn()?;
        let row = store::load(&mut conn, id)?;
        store::require_live(&row)?;
        store::require_generation(&row, request.expected_generation)?;
        if !store::rights(&mut conn, &row, principal.user)?.write {
            return Err(AppError::Forbidden);
        };
        row
    };
    let identity = Identity {
        organization: row.organization_id,
        secret: id,
        version: Uuid::new_v4(),
    };
    let envelope = state
        .secrets
        .crypto()?
        .seal(identity, &value)
        .await
        .map_err(crypto_error)?;
    {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            let row = lock(conn, id)?;
            store::require_live(&row)?;
            store::require_generation(&row, request.expected_generation)?;
            if !store::rights(conn, &row, principal.user)?.write {
                return Err(AppError::Forbidden);
            };
            let ordinal = secret_versions::table
                .filter(secret_versions::secret_id.eq(id))
                .select(diesel::dsl::max(secret_versions::ordinal))
                .first::<Option<i32>>(conn)?
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(AppError::Conflict("Version limit"))?;
            version_quota(conn, envelope.ciphertext.len())?;
            insert_version(conn, identity, ordinal, principal.user, &envelope)?;
            diesel::update(secrets::table.find(id))
                .set((
                    secrets::current_version_id.eq(Some(identity.version)),
                    secrets::acl_generation.eq(row.acl_generation + 1),
                    secrets::updated_at.eq(Utc::now()),
                ))
                .execute(conn)?;
            generation(conn, id, "version_created")?;
            store::audit(conn, principal.user, Some(id), "replace", "allowed")?;
            Ok(())
        })?;
    }
    state.secrets.refresh_redaction(&state.db_pool).await?;
    info(State(state), SecretPrincipal(principal), Path(id)).await
}
/// A recipient consents to masking their own data by explicitly requesting
/// the value, not merely by receiving an unsolicited share. Retain this rule
/// after revocation because the already-delivered copy may still exist.
fn register_reader(
    conn: &mut PgConnection,
    row: &SecretRow,
    version: &VersionRow,
    user: Uuid,
) -> Result<bool, AppError> {
    if row.namespace == "organization" || row.owner_user_id == user {
        return Ok(false);
    }
    use crate::schema::secret_occurrences as occurrences;
    let exists = |conn: &mut PgConnection| {
        occurrences::table
            .find((version.id, "reader", user.to_string()))
            .count()
            .get_result::<i64>(conn)
    };
    if exists(conn)? > 0 {
        return Ok(false);
    }
    conn.transaction::<_, AppError, _>(|conn| {
        secrets_generation::table
            .find(1i16)
            .for_update()
            .select(secrets_generation::generation)
            .first::<i64>(conn)?;
        if exists(conn)? > 0 {
            return Ok(false);
        }
        #[derive(QueryableByName)]
        struct Budget {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            bytes: i64,
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            count: i64,
        }
        let budget = diesel::sql_query(
            "SELECT ((SELECT COALESCE(SUM(octet_length(encrypted_value)),0) FROM secret_versions)
              + COALESCE(SUM(octet_length(v.encrypted_value)),0))::bigint AS bytes,
              COUNT(*)::bigint AS count
             FROM secret_occurrences o JOIN secret_versions v ON v.id=o.rule_version_id
             WHERE o.record_kind='reader'",
        )
        .get_result::<Budget>(conn)?;
        if budget.count >= 8192
            || budget
                .bytes
                .saturating_add(version.encrypted_value.len() as i64)
                > MAX_RULE_SOURCE_BYTES
        {
            return Err(AppError::ServiceUnavailable(
                "Secret reader redaction capacity reached",
            ));
        }
        diesel::insert_into(occurrences::table)
            .values((
                occurrences::rule_version_id.eq(version.id),
                occurrences::record_kind.eq("reader"),
                occurrences::record_id.eq(user.to_string()),
            ))
            .execute(conn)?;
        generation(conn, row.id, "reader_registered")?;
        Ok(true)
    })
}
async fn read(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    SecretJson(request): SecretJson<SecretReadRequest>,
) -> Result<Json<SecretReadResponse>, AppError> {
    auth::require_read(principal)?;
    state.secrets.refresh_redaction(&state.db_pool).await?;
    let (row, version) = {
        let mut conn = state.conn()?;
        let row = store::load(&mut conn, id)?;
        store::require_live(&row)?;
        let rights = store::rights(&mut conn, &row, principal.user)?;
        if !rights.read || (request.for_execution && !rights.use_value) {
            store::audit(&mut conn, principal.user, Some(id), "read", "denied")?;
            return Err(AppError::Forbidden);
        };
        let query = secret_versions::table.filter(secret_versions::secret_id.eq(id));
        let version = if let Some(ordinal) = request.version {
            query
                .filter(secret_versions::ordinal.eq(ordinal))
                .first::<VersionRow>(&mut conn)
        } else {
            query
                .filter(secret_versions::id.nullable().eq(row.current_version_id))
                .first::<VersionRow>(&mut conn)
        }
        .map_err(|_| AppError::NotFound("Secret version not found"))?;
        (row, version)
    };
    let value = state
        .secrets
        .crypto()?
        .open(
            Identity {
                organization: row.organization_id,
                secret: id,
                version: version.id,
            },
            &version.envelope(),
        )
        .await
        .map_err(crypto_error)?;
    // Recheck after the asynchronous key operation. A revoked credential, grant,
    // disabled account, deleted key or changed generation cannot release a value.
    let verified = auth::credential(&state, &headers)?.ok_or(AppError::Forbidden)?;
    auth::require_read(verified)?;
    {
        let mut conn = state.conn()?;
        let latest = store::load(&mut conn, id)?;
        store::require_live(&latest)?;
        store::require_generation(&latest, row.acl_generation)?;
        let rights = store::rights(&mut conn, &latest, verified.user)?;
        if verified.user != principal.user
            || !rights.read
            || (request.for_execution && !rights.use_value)
        {
            return Err(AppError::Forbidden);
        };
        store::audit(
            &mut conn,
            principal.user,
            Some(id),
            if request.for_execution {
                "execute_read"
            } else {
                "read"
            },
            "allowed",
        )?;
    }
    let added = {
        let mut conn = state.conn()?;
        register_reader(&mut conn, &row, &version, principal.user)?
    };
    if added {
        state.secrets.refresh_redaction(&state.db_pool).await?;
    }
    // Installing a newly consented reader rule is asynchronous. Revalidate
    // credentials and current policy once more before releasing plaintext.
    let verified = auth::credential(&state, &headers)?.ok_or(AppError::Forbidden)?;
    auth::require_read(verified)?;
    {
        let mut conn = state.conn()?;
        let latest = store::load(&mut conn, id)?;
        store::require_live(&latest)?;
        state.secrets.redaction.snapshot(&mut conn)?;
        store::require_generation(&latest, row.acl_generation)?;
        let rights = store::rights(&mut conn, &latest, verified.user)?;
        if verified.user != principal.user
            || !rights.read
            || (request.for_execution && !rights.use_value)
        {
            return Err(AppError::Forbidden);
        }
    }
    Ok(Json(SecretReadResponse {
        value: SecretValue(
            String::from_utf8(value.bytes().to_vec())
                .map_err(|_| AppError::ServiceUnavailable("Invalid secret encoding"))?,
        ),
        version: version.ordinal,
    }))
}
#[derive(serde::Deserialize)]
struct GenerationQuery {
    expected_generation: i64,
}
async fn grant(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
    SecretJson(request): SecretJson<SecretGrantRequest>,
) -> Result<Json<SecretMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    if principal.browser {
        auth::require_verification(&cookies, &state, principal.user)?
    }
    if request.rights.is_empty() {
        return Err(AppError::BadRequest("At least one right is required"));
    };
    {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            let row = lock(conn, id)?;
            store::require_live(&row)?;
            store::require_generation(&row, request.expected_generation)?;
            if !store::rights(conn, &row, principal.user)?.manage {
                return Err(AppError::Forbidden);
            };
            if row.namespace == "organization" {
                return Err(AppError::BadRequest(
                    "Organization secrets use fixed member read/use and curator management",
                ));
            }
            store::active_user(conn, request.user_id)?;
            let exists = secret_grants::table
                .find((id, request.user_id))
                .count()
                .get_result::<i64>(conn)?
                > 0;
            if !exists {
                // Short quota lock is separate from the archive rule fence,
                // so revocation never waits for transcript publication.
                diesel::sql_query("SELECT pg_advisory_xact_lock(1936024434, 1)").execute(conn)?;
                let per_key = secret_grants::table
                    .filter(secret_grants::secret_id.eq(id))
                    .count()
                    .get_result::<i64>(conn)?;
                if per_key >= 64 || secret_grants::table.count().get_result::<i64>(conn)? >= 8192 {
                    return Err(AppError::Conflict("Selected-user grant quota reached"));
                }
            }
            let rights = &request.rights;
            diesel::insert_into(secret_grants::table)
                .values((
                    secret_grants::secret_id.eq(id),
                    secret_grants::grantee_user_id.eq(request.user_id),
                    secret_grants::can_read.eq(rights.contains(&SecretRight::Read)),
                    secret_grants::can_use.eq(rights.contains(&SecretRight::Use)),
                    secret_grants::can_write.eq(rights.contains(&SecretRight::Write)),
                    secret_grants::can_manage.eq(rights.contains(&SecretRight::Manage)),
                    secret_grants::generation.eq(row.acl_generation + 1),
                ))
                .on_conflict((secret_grants::secret_id, secret_grants::grantee_user_id))
                .do_update()
                .set((
                    secret_grants::can_read.eq(rights.contains(&SecretRight::Read)),
                    secret_grants::can_use.eq(rights.contains(&SecretRight::Use)),
                    secret_grants::can_write.eq(rights.contains(&SecretRight::Write)),
                    secret_grants::can_manage.eq(rights.contains(&SecretRight::Manage)),
                    secret_grants::generation.eq(row.acl_generation + 1),
                    secret_grants::expires_at.eq(None::<chrono::DateTime<Utc>>),
                ))
                .execute(conn)?;
            store::record_acl_change(
                conn,
                id,
                principal.user,
                request.user_id,
                store::Rights {
                    read: rights.contains(&SecretRight::Read),
                    use_value: rights.contains(&SecretRight::Use),
                    write: rights.contains(&SecretRight::Write),
                    manage: rights.contains(&SecretRight::Manage),
                },
                row.acl_generation + 1,
                "grant",
            )?;
            bump_acl(conn, &row, "grant", principal.user)?;
            Ok(())
        })?;
    };
    info(State(state), SecretPrincipal(principal), Path(id)).await
}
async fn revoke(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    Path((id, user)): Path<(Uuid, Uuid)>,
    Query(request): Query<GenerationQuery>,
) -> Result<Json<SecretMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            let row = lock(conn, id)?;
            store::require_generation(&row, request.expected_generation)?;
            if !store::rights(conn, &row, principal.user)?.manage {
                return Err(AppError::Forbidden);
            };
            if let Some(previous) = secret_grants::table
                .find((id, user))
                .first::<store::GrantRow>(conn)
                .optional()?
            {
                store::record_acl_change(
                    conn,
                    id,
                    principal.user,
                    user,
                    store::Rights {
                        read: previous.can_read,
                        use_value: previous.can_use,
                        write: previous.can_write,
                        manage: previous.can_manage,
                    },
                    row.acl_generation + 1,
                    "revoke",
                )?;
            }
            diesel::delete(secret_grants::table.find((id, user))).execute(conn)?;
            bump_acl(conn, &row, "revoke", principal.user)?;
            Ok(())
        })?;
    };
    info(State(state), SecretPrincipal(principal), Path(id)).await
}
fn bump_acl(
    conn: &mut PgConnection,
    row: &SecretRow,
    action: &str,
    user: Uuid,
) -> Result<(), AppError> {
    diesel::update(secrets::table.find(row.id))
        .set((
            secrets::acl_generation.eq(row.acl_generation + 1),
            secrets::updated_at.eq(Utc::now()),
        ))
        .execute(conn)?;
    use crate::schema::secret_bindings as bindings;
    diesel::update(bindings::table.filter(bindings::secret_id.eq(row.id)))
        .set(bindings::revoked_at.eq(Some(Utc::now())))
        .execute(conn)?;
    // ACL changes do not alter the retained redaction rule set. Keeping
    // their outbox generation per key prevents a long archive publication
    // fence from delaying grant revocation or reversible deletion.
    use crate::schema::secret_outbox as outbox;
    diesel::insert_into(outbox::table)
        .values((
            outbox::id.eq(Uuid::new_v4()),
            outbox::kind.eq(action),
            outbox::target_id.eq(row.id),
            outbox::generation.eq(row.acl_generation + 1),
        ))
        .execute(conn)?;
    store::audit(conn, user, Some(row.id), action, "allowed")
}
async fn soft_delete(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    SecretJson(request): SecretJson<SecretMutationRequest>,
) -> Result<Json<SecretMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            let row = lock(conn, id)?;
            store::require_generation(&row, request.expected_generation)?;
            if !store::rights(conn, &row, principal.user)?.manage {
                return Err(AppError::Forbidden);
            };
            let mut tags = row.system_tags.clone();
            if !tags.iter().flatten().any(|tag| tag == "deleted") {
                tags.push(Some("deleted".into()))
            };
            diesel::update(secrets::table.find(id))
                .set((
                    secrets::system_tags.eq(tags),
                    secrets::deleted_at.eq(Some(Utc::now())),
                    secrets::deleted_by.eq(Some(principal.user)),
                ))
                .execute(conn)?;
            bump_acl(conn, &row, "delete", principal.user)?;
            Ok(())
        })?;
    };
    info(State(state), SecretPrincipal(principal), Path(id)).await
}
async fn restore(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
    SecretJson(request): SecretJson<SecretRestoreRequest>,
) -> Result<Json<SecretMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    if principal.browser {
        auth::require_verification(&cookies, &state, principal.user)?
    }
    if !request.confirm_sharing {
        return Err(AppError::BadRequest(
            "Confirm the retained sharing audience before restore",
        ));
    };
    {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            let row = lock(conn, id)?;
            store::require_generation(&row, request.expected_generation)?;
            if !store::rights(conn, &row, principal.user)?.manage {
                return Err(AppError::Forbidden);
            };
            if row.namespace == "organization" && !store::curator(conn, principal.user)? {
                return Err(AppError::Forbidden);
            };
            let tags: Vec<_> = row
                .system_tags
                .iter()
                .filter(|tag| tag.as_deref() != Some("deleted"))
                .cloned()
                .collect();
            diesel::update(secrets::table.find(id))
                .set((
                    secrets::system_tags.eq(tags),
                    secrets::deleted_at.eq(None::<chrono::DateTime<Utc>>),
                    secrets::deleted_by.eq(None::<Uuid>),
                ))
                .execute(conn)?;
            bump_acl(conn, &row, "restore", principal.user)?;
            Ok(())
        })?;
    };
    info(State(state), SecretPrincipal(principal), Path(id)).await
}
async fn no_store(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::PRAGMA,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response
}
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    idempotency::layer(
        Router::new()
            .route("/api/secrets", get(list).post(create))
            .route(
                "/api/secrets/bindings",
                get(bindings::list).post(bindings::create),
            )
            .route("/api/secrets/bindings/{id}/revoke", post(bindings::revoke))
            .route("/api/secrets/bindings/{id}/ticket", post(bindings::ticket))
            .route("/api/secrets/bindings/{id}/redeem", post(bindings::redeem))
            .route("/api/secrets/members", get(members::list))
            .route("/api/secrets/members/resolve", post(members::resolve))
            .route("/api/secrets/resolve", post(resolve))
            .route("/api/secrets/{id}", get(info))
            .route("/api/secrets/{id}/versions", get(versions).post(write))
            .route("/api/secrets/{id}/read", post(read))
            .route("/api/secrets/{id}/grants", post(grant))
            .route("/api/secrets/{id}/grants/{user}", delete(revoke))
            .route("/api/secrets/{id}/delete", post(soft_delete))
            .route("/api/secrets/{id}/restore", post(restore))
            .route(
                "/api/secrets/{id}/expunge",
                post(crate::secrets::expunge::enqueue),
            )
            .route(
                "/api/secrets/jobs/{id}",
                get(crate::secrets::expunge::get_job),
            )
            .merge(enrollment::router())
            .merge(access_requests::router())
            .layer(DefaultBodyLimit::max(400_000)),
        state,
    )
    .layer(axum::middleware::from_fn(no_store))
}

#[cfg(test)]
#[path = "secrets/vault_tests.rs"]
pub(crate) mod tests;

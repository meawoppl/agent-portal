//! Store only actor/key/path/target, never request bodies or value fingerprints.
//! Replays reauthorize and render current metadata, so revocation is immediate.
use super::*;
use crate::schema::secret_mutations as receipts;
use diesel::sql_types::{BigInt, Bool};
struct Lock {
    conn: crate::db::DbConnection,
    key: i64,
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = diesel::sql_query("SELECT pg_advisory_unlock($1)")
            .bind::<BigInt, _>(self.key)
            .execute(&mut self.conn);
    }
}
#[derive(QueryableByName)]
struct Acquired {
    #[diesel(sql_type=Bool)]
    acquired: bool,
}
pub(super) fn key(headers: &HeaderMap) -> Result<Uuid, AppError> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or(AppError::BadRequest(
            "A UUID Idempotency-Key header is required",
        ))
}
async fn handle(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method();
    let protected = (method == axum::http::Method::POST || method == axum::http::Method::DELETE)
        && !path.contains("/authorize/")
        && path != "/api/secrets/request-groups"
        && !path.starts_with("/api/secrets/request-groups/")
        && path != "/api/secrets/requests"
        && !path.starts_with("/api/secrets/requests/")
        && !path.ends_with("/read")
        && !path.ends_with("/copy")
        && !path.ends_with("/resolve")
        && !path.ends_with("/expunge")
        && !(path.contains("/bindings/")
            && (path.ends_with("/ticket") || path.ends_with("/redeem")));
    if !protected {
        return next.run(request).await;
    }
    let auth = (|| -> Result<_, AppError> {
        let cookies = request
            .extensions()
            .get::<tower_cookies::Cookies>()
            .ok_or(AppError::Unauthorized)?;
        Ok((
            principal(&state, request.headers(), cookies, true)?,
            key(request.headers())?,
        ))
    })();
    let (actor, key) = match auth {
        Ok(pair) => pair,
        Err(error) => return error.into_response(),
    };
    // Read a bounded complete body before reserving a mutation slot or a lock
    // connection. Slow clients cannot monopolize the write gate indefinitely.
    let body_permit = match state.secrets.body_gate.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return AppError::ServiceUnavailable("Secret request capacity reached; retry")
                .into_response()
        }
    };
    let (parts, body) = request.into_parts();
    let bytes = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        axum::body::to_bytes(body, 400_000),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return AppError::PayloadTooLarge("Secret request exceeds the body limit".into())
                .into_response()
        }
        Err(_) => return AppError::GatewayTimeout("Secret request body timed out").into_response(),
    };
    drop(body_permit);
    let request = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
    // Reserve mutation capacity before borrowing a pool connection. Otherwise
    // N lock holders can starve all N handlers waiting for a second checkout.
    if state.db_pool.max_size() < 2 {
        return AppError::ServiceUnavailable("Secrets mutations require two database connections")
            .into_response();
    }
    let _permit = match state.secrets.mutation_gate.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return AppError::ServiceUnavailable("Another secrets mutation is in progress; retry")
                .into_response()
        }
    };
    let setup = (|| -> Result<_, AppError> {
        let mut conn = state.conn()?;
        let lock_uuid = Uuid::new_v5(&actor.user, key.as_bytes());
        let mut lock_bytes = [0u8; 8];
        lock_bytes.copy_from_slice(&lock_uuid.as_bytes()[..8]);
        let lock_key = i64::from_be_bytes(lock_bytes);
        let locked = diesel::sql_query("SELECT pg_try_advisory_lock($1) AS acquired")
            .bind::<BigInt, _>(lock_key)
            .get_result::<Acquired>(&mut conn)?
            .acquired;
        if !locked {
            return Err(AppError::Conflict("Identical mutation is in progress"));
        };
        Ok((
            actor,
            key,
            Lock {
                conn,
                key: lock_key,
            },
        ))
    })();
    let (actor, key, mut guard) = match setup {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let cached = receipts::table
        .find((actor.user, key))
        .select((receipts::path, receipts::secret_id, receipts::binding_id))
        .first::<(String, Uuid, Option<Uuid>)>(&mut guard.conn)
        .optional();
    match cached {
        Ok(Some((old_path, id, binding))) => {
            if old_path != path {
                return AppError::Conflict("Idempotency key belongs to another operation")
                    .into_response();
            };
            if let Some(binding) = binding {
                return match super::bindings::replay_metadata(&mut guard.conn, binding, actor.user)
                {
                    Ok(metadata) => Json(metadata).into_response(),
                    Err(error) => error.into_response(),
                };
            }
            return match store::load(&mut guard.conn, id)
                .and_then(|row| store::metadata(&mut guard.conn, &row, actor.user))
            {
                Ok(metadata) => Json(metadata).into_response(),
                Err(error) => error.into_response(),
            };
        }
        Err(error) => return AppError::from(error).into_response(),
        _ => {}
    }
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return AppError::Internal("Secret metadata response unavailable".into())
                .into_response()
        }
    };
    let target = if let Ok(metadata) = serde_json::from_slice::<SecretMetadata>(&bytes) {
        Some((metadata.id, None))
    } else if let Ok(metadata) = serde_json::from_slice::<SecretBindingMetadata>(&bytes) {
        Some((metadata.secret_id, Some(metadata.id)))
    } else {
        None
    };
    if let Some((secret, binding)) = target {
        if diesel::insert_into(receipts::table)
            .values((
                receipts::actor_user_id.eq(actor.user),
                receipts::request_id.eq(key),
                receipts::path.eq(path),
                receipts::secret_id.eq(secret),
                receipts::binding_id.eq(binding),
            ))
            .execute(&mut guard.conn)
            .is_err()
        {
            return AppError::ServiceUnavailable(
                "Secret mutation committed; reload metadata before retrying",
            )
            .into_response();
        }
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
}
pub(super) fn layer(router: Router<Arc<AppState>>, state: Arc<AppState>) -> Router<Arc<AppState>> {
    router.layer(axum::middleware::from_fn_with_state(state, handle))
}

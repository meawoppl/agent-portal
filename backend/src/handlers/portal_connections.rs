//! The home portal's synced list of a user's linked accounts on other portals
//! (`docs/design/FEDERATION_DISCUSSION.md`). Metadata only: no credential for
//! a linked portal is ever sent here, and the stored scope is a display hint.

use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use shared::api::{
    PortalConnection, PortalConnectionsResponse, TokenScope, UpdatePortalConnectionRequest,
    UpsertPortalConnectionRequest,
};
use shared::urls::canonical_origin;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::responses::EmptyResponse;
use crate::models::{self, NewPortalConnection};
use crate::schema::{portal_connections, users};
use crate::AppState;

/// Linked accounts per user. Generous for one person across work and personal
/// portals; a bound so a misbehaving client can't grow the table unchecked.
const MAX_CONNECTIONS_PER_USER: i64 = 32;
const MAX_LABEL_CHARS: usize = 64;
const MAX_EMAIL_CHARS: usize = 255;

/// `GET /api/portal-connections`
pub async fn list_portal_connections(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<PortalConnectionsResponse>, AppError> {
    let mut conn = app_state.conn()?;
    let connections = portal_connections::table
        .filter(portal_connections::user_id.eq(user_id))
        .order((
            portal_connections::label.asc(),
            portal_connections::created_at.asc(),
        ))
        .select(models::PortalConnection::as_select())
        .load(&mut conn)?
        .into_iter()
        .filter_map(to_wire)
        .collect();
    Ok(Json(PortalConnectionsResponse { connections }))
}

/// `POST /api/portal-connections` — record a completed enrollment, or refresh
/// the stored metadata of an existing `(portal_origin, remote_user_id)` link.
pub async fn upsert_portal_connection(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(request): Json<UpsertPortalConnectionRequest>,
) -> Result<Json<PortalConnection>, AppError> {
    let portal_origin = canonical_origin(&request.portal_origin)
        .map_err(|_| AppError::BadRequest("portal_origin is not a valid origin"))?;
    if canonical_origin(&app_state.public_url).as_deref() == Ok(portal_origin.as_str()) {
        return Err(AppError::BadRequest("cannot link a portal to itself"));
    }
    let label = validated_label(&request.label)?;
    let remote_email = request.remote_email.trim();
    if remote_email.is_empty() || remote_email.chars().count() > MAX_EMAIL_CHARS {
        return Err(AppError::BadRequest(
            "remote_email must be 1-255 characters",
        ));
    }
    let row = NewPortalConnection {
        user_id,
        portal_origin,
        instance_id: request.instance_id,
        remote_user_id: request.remote_user_id,
        remote_email: remote_email.to_string(),
        label,
        scope: request.scope.as_str().to_string(),
    };

    let mut conn = app_state.conn()?;
    let stored = conn.transaction::<_, AppError, _>(|conn| {
        // Serialize this user's upserts so concurrent new links can't both
        // pass the cap check.
        users::table
            .find(user_id)
            .select(users::id)
            .for_update()
            .first::<Uuid>(conn)?;
        let existing = portal_connections::table
            .filter(portal_connections::user_id.eq(user_id))
            .filter(portal_connections::portal_origin.eq(&row.portal_origin))
            .filter(portal_connections::remote_user_id.eq(row.remote_user_id))
            .select(portal_connections::id)
            .first::<Uuid>(conn)
            .optional()?;
        if existing.is_none() {
            let count = portal_connections::table
                .filter(portal_connections::user_id.eq(user_id))
                .count()
                .get_result::<i64>(conn)?;
            if count >= MAX_CONNECTIONS_PER_USER {
                return Err(AppError::Conflict("too many linked portals"));
            }
        }
        Ok(diesel::insert_into(portal_connections::table)
            .values(&row)
            .on_conflict((
                portal_connections::user_id,
                portal_connections::portal_origin,
                portal_connections::remote_user_id,
            ))
            .do_update()
            .set((&row, portal_connections::updated_at.eq(diesel::dsl::now)))
            .returning(models::PortalConnection::as_returning())
            .get_result(conn)?)
    })?;
    to_wire(stored)
        .map(Json)
        .ok_or_else(|| AppError::Internal("stored scope did not round-trip".into()))
}

/// `PATCH /api/portal-connections/{id}` — rename a connection.
pub async fn update_portal_connection(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(connection_id): Path<Uuid>,
    Json(request): Json<UpdatePortalConnectionRequest>,
) -> Result<Json<PortalConnection>, AppError> {
    let label = validated_label(&request.label)?;
    let mut conn = app_state.conn()?;
    let updated = diesel::update(
        portal_connections::table
            .find(connection_id)
            .filter(portal_connections::user_id.eq(user_id)),
    )
    .set((
        portal_connections::label.eq(label),
        portal_connections::updated_at.eq(diesel::dsl::now),
    ))
    .returning(models::PortalConnection::as_returning())
    .get_result(&mut conn)
    .optional()?
    .ok_or(AppError::NotFound("portal connection not found"))?;
    to_wire(updated)
        .map(Json)
        .ok_or_else(|| AppError::Internal("stored scope did not round-trip".into()))
}

/// `DELETE /api/portal-connections/{id}` — forget a connection on the home.
/// Revoking the grant on the linked portal is the client's job (it holds the
/// credential); this only removes the synced record.
pub async fn delete_portal_connection(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(connection_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;
    let deleted = diesel::delete(
        portal_connections::table
            .find(connection_id)
            .filter(portal_connections::user_id.eq(user_id)),
    )
    .execute(&mut conn)?;
    if deleted == 0 {
        return Err(AppError::NotFound("portal connection not found"));
    }
    Ok(EmptyResponse::NO_CONTENT)
}

fn validated_label(label: &str) -> Result<String, AppError> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(AppError::BadRequest("label must be 1-64 characters"));
    }
    Ok(label.to_string())
}

/// Rows with a scope this build doesn't know are skipped rather than guessed.
fn to_wire(row: models::PortalConnection) -> Option<PortalConnection> {
    Some(PortalConnection {
        id: row.id,
        portal_origin: row.portal_origin,
        instance_id: row.instance_id,
        remote_user_id: row.remote_user_id,
        remote_email: row.remote_email,
        label: row.label,
        scope: TokenScope::parse(&row.scope)?,
        created_at: row.created_at.and_utc().to_rfc3339(),
        updated_at: row.updated_at.and_utc().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::validated_label;

    #[test]
    fn labels_are_trimmed_and_bounded() {
        assert_eq!(validated_label("  work ").ok().as_deref(), Some("work"));
        assert!(validated_label("   ").is_err());
        assert!(validated_label(&"x".repeat(65)).is_err());
        assert!(validated_label(&"é".repeat(64)).is_ok());
    }
}

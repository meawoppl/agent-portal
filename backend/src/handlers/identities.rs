//! Self-service linked-login settings (#1535).

use axum::{extract::Path, extract::State, Json};
use diesel::prelude::*;
use shared::api::{LinkedIdentitiesResponse, LinkedIdentity};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::responses::EmptyResponse;
use crate::models::UserIdentity;
use crate::schema::{user_identities, users};
use crate::AppState;

/// `GET /api/settings/identities` — list the caller's linked login methods.
pub async fn list_identities(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<LinkedIdentitiesResponse>, AppError> {
    let mut conn = app_state.conn()?;
    let identities = user_identities::table
        .filter(user_identities::user_id.eq(user_id))
        .order(user_identities::created_at.asc())
        .select(UserIdentity::as_select())
        .load::<UserIdentity>(&mut conn)?
        .into_iter()
        .map(|identity| LinkedIdentity {
            id: identity.id,
            provider: identity.provider,
            email: identity.email,
            linked_at: identity.created_at.and_utc().to_rfc3339(),
        })
        .collect();

    Ok(Json(LinkedIdentitiesResponse { identities }))
}

/// `DELETE /api/settings/identities/:id` — unlink one login while preserving
/// at least one way back into the account.
pub async fn unlink_identity(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(identity_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;
    conn.transaction::<_, AppError, _>(|conn| {
        // Serialize concurrent unlink attempts for this account. Without the
        // user-row lock, two requests could both observe a count of two and
        // each delete one, leaving the account with no login method.
        users::table
            .find(user_id)
            .select(users::id)
            .for_update()
            .first::<Uuid>(conn)?;

        let identity_count = user_identities::table
            .filter(user_identities::user_id.eq(user_id))
            .count()
            .get_result::<i64>(conn)?;
        ensure_can_unlink(identity_count)?;

        let deleted = diesel::delete(
            user_identities::table
                .find(identity_id)
                .filter(user_identities::user_id.eq(user_id)),
        )
        .execute(conn)?;
        if deleted == 0 {
            return Err(AppError::NotFound("Linked identity not found"));
        }
        Ok(())
    })?;

    Ok(EmptyResponse::OK)
}

fn ensure_can_unlink(identity_count: i64) -> Result<(), AppError> {
    if identity_count <= 1 {
        Err(AppError::BadRequest("Cannot unlink the last login method"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ensure_can_unlink;
    use crate::errors::AppError;

    #[test]
    fn last_login_method_cannot_be_unlinked() {
        assert!(matches!(
            ensure_can_unlink(1),
            Err(AppError::BadRequest("Cannot unlink the last login method"))
        ));
        assert!(ensure_can_unlink(2).is_ok());
    }
}

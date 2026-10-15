//! Federation discovery: what a linked client needs to know before it talks
//! to this portal (`docs/design/FEDERATION_DISCUSSION.md`).

use axum::{extract::State, Json};
use diesel::prelude::*;
use shared::api::{FederationDiscoveryResponse, FEDERATION_PROTOCOL};
use std::sync::Arc;

use crate::errors::AppError;
use crate::schema::portal_instance;
use crate::AppState;

/// `GET /api/federation/discovery` — unauthenticated, so a client can decide
/// whether this portal is usable before asking the user to enroll.
///
/// Advertise only what is enforced end to end: an empty capability list is
/// how this portal says "no linked clients", and clients must not enroll or
/// open tickets against capabilities that aren't listed.
pub async fn discovery(
    State(app_state): State<Arc<AppState>>,
) -> Result<Json<FederationDiscoveryResponse>, AppError> {
    let mut conn = app_state.conn()?;
    let instance_id = portal_instance::table
        .select(portal_instance::instance_id)
        .first(&mut conn)?;
    Ok(Json(FederationDiscoveryResponse {
        instance_id,
        protocol: FEDERATION_PROTOCOL,
        capabilities: Vec::new(),
        auth_modes: Vec::new(),
    }))
}

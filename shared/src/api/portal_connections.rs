//! The home portal's record of a user's linked accounts on other portals.
//!
//! Metadata only — the credential for a connection stays on the device that
//! enrolled it, so a new device sees the connection and re-authorizes it.
//! Syncing is optional: a device-only connection never reaches these routes.
//! These records disclose the user's affiliations to the home operator, and
//! the scope here is a display hint, never authority.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::TokenScope;

/// One linked account, as stored by the home portal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalConnection {
    pub id: Uuid,
    /// Canonical origin of the linked portal (`shared::urls::canonical_origin`).
    pub portal_origin: String,
    /// The linked portal's discovery `instance_id` at enrollment.
    pub instance_id: Uuid,
    /// The account's id on the linked portal; never compared with local ids.
    pub remote_user_id: Uuid,
    /// Display only; never an identity-linking key.
    pub remote_email: String,
    pub label: String,
    pub scope: TokenScope,
    /// RFC 3339 UTC.
    pub created_at: String,
    /// RFC 3339 UTC.
    pub updated_at: String,
}

/// `GET /api/portal-connections`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalConnectionsResponse {
    pub connections: Vec<PortalConnection>,
}

/// `POST /api/portal-connections`: record (or refresh) a connection after a
/// completed enrollment. Upserts on `(portal_origin, remote_user_id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertPortalConnectionRequest {
    pub portal_origin: String,
    pub instance_id: Uuid,
    pub remote_user_id: Uuid,
    pub remote_email: String,
    pub label: String,
    pub scope: TokenScope,
}

/// `PATCH /api/portal-connections/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePortalConnectionRequest {
    pub label: String,
}

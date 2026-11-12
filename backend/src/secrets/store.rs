use crate::{
    errors::AppError,
    schema::{secret_grants, secret_versions, secrets},
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use shared::api::*;
use uuid::Uuid;
#[derive(Queryable, Selectable, Clone)]
#[diesel(table_name=secrets)]
pub struct SecretRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub owner_user_id: Uuid,
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub current_version_id: Option<Uuid>,
    pub system_tags: Vec<Option<String>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_by: Option<Uuid>,
    pub acl_generation: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Queryable, Selectable, Clone)]
#[diesel(table_name=secret_versions)]
pub struct VersionRow {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub ordinal: i32,
    pub encrypted_value: Vec<u8>,
    pub nonce: Vec<u8>,
    pub auth_tag: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
    pub wrapping_key_id: String,
    pub crypto_format: i32,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}
impl VersionRow {
    pub fn envelope(&self) -> super::crypto::Envelope {
        super::crypto::Envelope {
            ciphertext: self.encrypted_value.clone(),
            nonce: self.nonce.clone(),
            tag: self.auth_tag.clone(),
            wrapped_dek: self.wrapped_dek.clone(),
            wrapping_key_id: self.wrapping_key_id.clone(),
            crypto_format: self.crypto_format,
        }
    }
}
#[derive(Queryable, Selectable)]
#[diesel(table_name=secret_grants)]
pub struct GrantRow {
    pub secret_id: Uuid,
    pub grantee_user_id: Uuid,
    pub can_read: bool,
    pub can_use: bool,
    pub can_write: bool,
    pub can_manage: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub generation: i64,
}
#[derive(Clone, Copy, Default)]
pub struct Rights {
    pub read: bool,
    pub use_value: bool,
    pub write: bool,
    pub manage: bool,
}
impl Rights {
    pub fn any(self) -> bool {
        self.read || self.use_value || self.write || self.manage
    }
}
pub fn active_user(conn: &mut PgConnection, user: Uuid) -> Result<crate::models::User, AppError> {
    use crate::schema::users;
    users::table
        .find(user)
        .filter(users::disabled.eq(false))
        .first(conn)
        .map_err(|_| AppError::Forbidden)
}
pub fn curator(conn: &mut PgConnection, user: Uuid) -> Result<bool, AppError> {
    Ok(active_user(conn, user)?.is_admin)
}
pub fn rights(conn: &mut PgConnection, row: &SecretRow, user: Uuid) -> Result<Rights, AppError> {
    let actor = active_user(conn, user)?;
    if row.namespace == "organization" {
        return Ok(Rights {
            read: true,
            use_value: true,
            write: actor.is_admin,
            manage: actor.is_admin,
        });
    }
    if row.owner_user_id == user {
        return Ok(Rights {
            read: true,
            use_value: true,
            write: true,
            manage: true,
        });
    };
    let mut result = Rights::default();
    if let Some(g) = secret_grants::table
        .find((row.id, user))
        .first::<GrantRow>(conn)
        .optional()?
    {
        if g.expires_at.is_none_or(|time| time > Utc::now()) {
            result.read = g.can_read;
            result.use_value = g.can_use;
            result.write = g.can_write;
            result.manage = g.can_manage
        }
    };
    Ok(result)
}

pub fn load(conn: &mut PgConnection, id: Uuid) -> Result<SecretRow, AppError> {
    secrets::table
        .find(id)
        .first(conn)
        .map_err(|_| AppError::NotFound("Secret not found"))
}
pub fn metadata(
    conn: &mut PgConnection,
    row: &SecretRow,
    user: Uuid,
) -> Result<SecretMetadata, AppError> {
    let rights = rights(conn, row, user)?;
    if !rights.any() {
        return Err(AppError::NotFound("Secret not found"));
    };
    let grants = if rights.manage {
        secret_grants::table
            .filter(secret_grants::secret_id.eq(row.id))
            .load::<GrantRow>(conn)?
            .into_iter()
            .map(|g| {
                let mut r = vec![];
                if g.can_read {
                    r.push(SecretRight::Read)
                };
                if g.can_use {
                    r.push(SecretRight::Use)
                };
                if g.can_write {
                    r.push(SecretRight::Write)
                };
                if g.can_manage {
                    r.push(SecretRight::Manage)
                };
                SecretGrant {
                    user_id: g.grantee_user_id,
                    rights: r,
                }
            })
            .collect()
    } else {
        vec![]
    };
    let current_version = match row.current_version_id {
        Some(id) => secret_versions::table
            .find(id)
            .select(secret_versions::ordinal)
            .first(conn)?,
        None => 0,
    };
    Ok(SecretMetadata {
        id: row.id,
        owner_user_id: row.owner_user_id,
        namespace: if row.namespace == "personal" {
            SecretNamespace::Personal
        } else {
            SecretNamespace::Organization
        },
        name: row.name.clone(),
        description: row.description.clone(),
        tags: row.system_tags.iter().flatten().cloned().collect(),
        current_version,
        generation: row.acl_generation,
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
        deleted_at: row.deleted_at.map(|t| t.to_rfc3339()),
        deleted_by: row.deleted_by,
        can_read: rights.read,
        can_use: rights.use_value,
        can_write: rights.write,
        can_manage: rights.manage,
        grants,
    })
}
pub fn audit(
    conn: &mut PgConnection,
    actor: Uuid,
    target: Option<Uuid>,
    action: &str,
    outcome: &str,
) -> Result<(), AppError> {
    use crate::schema::secret_audit_events as table;
    diesel::insert_into(table::table)
        .values((
            table::id.eq(Uuid::new_v4()),
            table::actor_user_id.eq(Some(actor)),
            table::secret_id.eq(target),
            table::action.eq(action),
            table::outcome.eq(outcome),
        ))
        .execute(conn)?;
    Ok(())
}
pub fn require_generation(row: &SecretRow, expected: i64) -> Result<(), AppError> {
    if row.acl_generation != expected {
        Err(AppError::Conflict("Secret changed; reload metadata"))
    } else {
        Ok(())
    }
}
pub fn require_live(row: &SecretRow) -> Result<(), AppError> {
    if row.deleted_at.is_some() || row.system_tags.iter().flatten().any(|s| s == "deleted") {
        Err(AppError::Conflict("Secret is deleted"))
    } else {
        Ok(())
    }
}

pub fn require_manage(
    conn: &mut PgConnection,
    secret: Uuid,
    user: Uuid,
) -> Result<SecretRow, AppError> {
    let row = load(conn, secret)?;
    if !rights(conn, &row, user)?.manage {
        return Err(AppError::Forbidden);
    };
    Ok(row)
}

/// Preserve prior recipients and rights even after the live grant is revoked.
pub fn record_acl_change(
    conn: &mut PgConnection,
    secret: Uuid,
    actor: Uuid,
    grantee: Uuid,
    rights: Rights,
    generation: i64,
    action: &str,
) -> Result<(), AppError> {
    use crate::schema::secret_acl_history as history;
    diesel::insert_into(history::table)
        .values((
            history::id.eq(Uuid::new_v4()),
            history::secret_id.eq(secret),
            history::actor_user_id.eq(actor),
            history::grantee_user_id.eq(grantee),
            history::action.eq(action),
            history::generation.eq(generation),
            history::can_read.eq(rights.read),
            history::can_use.eq(rights.use_value),
            history::can_write.eq(rights.write),
            history::can_manage.eq(rights.manage),
        ))
        .execute(conn)?;
    Ok(())
}

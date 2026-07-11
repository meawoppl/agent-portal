//! Admin custom-subdomain management (docs/PORT_FORWARDING.md). An admin can
//! give a session's forward a human-readable alias that routes alongside its
//! auto 8-hex label. All endpoints are admin-only (`require_admin`).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use tower_cookies::Cookies;
use tracing::info;
use uuid::Uuid;

use shared::api::{
    AdminForwardInfo, AdminForwardsResponse, CreateCustomSubdomainRequest, CustomSubdomainInfo,
    CustomSubdomainsResponse,
};

use crate::errors::AppError;
use crate::handlers::admin::require_admin;
use crate::handlers::forward_proxy::is_reserved_label;
use crate::models::NewCustomSubdomain;
use crate::AppState;

/// Build the public URL for a subdomain label. Errors when forwarding is off.
fn label_url(app_state: &AppState, label: &str) -> Result<String, AppError> {
    let domain = app_state
        .forward_domain
        .as_deref()
        .ok_or(AppError::ServiceUnavailable(
            "Forwarding is not configured on this server",
        ))?;
    let scheme = if app_state.public_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    Ok(format!("{scheme}://{label}.{domain}/"))
}

/// Validate a candidate custom label: a single lowercase DNS label (1–63
/// chars, `[a-z0-9-]`, no leading/trailing hyphen), not an 8-hex string (that
/// namespace belongs to auto labels), and not a reserved word. Returns a
/// human-readable reason on rejection so the admin's text entry can show it.
fn validate_custom_label(label: &str) -> Result<(), &'static str> {
    if label.is_empty() || label.len() > 63 {
        return Err("subdomain must be 1–63 characters");
    }
    if !label
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("subdomain may only contain lowercase letters, digits, and hyphens");
    }
    if label.starts_with('-') || label.ends_with('-') {
        return Err("subdomain may not start or end with a hyphen");
    }
    // Auto labels are exactly 8 hex chars; keep that namespace separate so a
    // custom label can never collide with a generated one.
    if label.len() == 8 && label.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("subdomain looks like a generated label; choose a different name");
    }
    if is_reserved_label(label) {
        return Err("that subdomain is reserved");
    }
    Ok(())
}

/// GET /api/admin/subdomains — all custom subdomains, newest first.
pub async fn list_custom_subdomains(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<CustomSubdomainsResponse>, AppError> {
    require_admin(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;

    use crate::schema::{custom_subdomains as cs, sessions};
    let rows: Vec<(String, Uuid, String, chrono::NaiveDateTime)> = cs::table
        .inner_join(sessions::table.on(sessions::id.eq(cs::session_id)))
        .select((
            cs::label,
            cs::session_id,
            sessions::session_name,
            cs::created_at,
        ))
        .order(cs::created_at.desc())
        .load(&mut conn)?;

    let subdomains = rows
        .into_iter()
        .map(|(label, session_id, session_name, created_at)| {
            Ok(CustomSubdomainInfo {
                url: label_url(&app_state, &label)?,
                label,
                session_id,
                session_name,
                created_at: DateTime::<Utc>::from_naive_utc_and_offset(created_at, Utc)
                    .to_rfc3339(),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(CustomSubdomainsResponse { subdomains }))
}

/// POST /api/admin/subdomains — assign a custom subdomain to a session's
/// forward. Deconflicts explicitly: an invalid label is a 400 with the reason;
/// a taken label or a session that already has one is a 409.
pub async fn create_custom_subdomain(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<CreateCustomSubdomainRequest>,
) -> Result<(StatusCode, Json<CustomSubdomainInfo>), AppError> {
    let admin = require_admin(&app_state, &headers, &cookies)?;
    let label = req.label.trim().to_ascii_lowercase();
    validate_custom_label(&label).map_err(AppError::BadRequest)?;
    if app_state.forward_domain.is_none() {
        return Err(AppError::ServiceUnavailable(
            "Forwarding is not configured on this server",
        ));
    }

    let mut conn = app_state.conn()?;
    use crate::schema::{custom_subdomains as cs, sessions};

    // The session must exist and have an active forward (the alias points at a
    // forward).
    let session_name: String = sessions::table
        .find(req.session_id)
        .select(sessions::session_name)
        .first::<String>(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("session"))?;
    if crate::handlers::forwards::active_forward_port(&mut conn, req.session_id)?.is_none() {
        return Err(AppError::BadRequest("session has no active forward"));
    }

    // Explicit deconfliction with clear messages (the PK / session UNIQUE are
    // the backstop for the race).
    let label_taken = cs::table
        .filter(cs::label.eq(&label))
        .select(cs::label)
        .first::<String>(&mut conn)
        .optional()?
        .is_some();
    if label_taken {
        return Err(AppError::Conflict("that subdomain is already in use"));
    }
    let session_has_one = cs::table
        .filter(cs::session_id.eq(req.session_id))
        .select(cs::label)
        .first::<String>(&mut conn)
        .optional()?
        .is_some();
    if session_has_one {
        return Err(AppError::Conflict(
            "that session already has a custom subdomain",
        ));
    }

    diesel::insert_into(cs::table)
        .values(&NewCustomSubdomain {
            label: label.clone(),
            session_id: req.session_id,
            created_by: Some(admin.id),
        })
        .execute(&mut conn)
        .map_err(|e| match e {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::Conflict("that subdomain is already in use"),
            other => AppError::from(other),
        })?;

    info!(
        "Admin {} assigned custom subdomain '{}' to session {}",
        admin.email, label, req.session_id
    );

    Ok((
        StatusCode::CREATED,
        Json(CustomSubdomainInfo {
            url: label_url(&app_state, &label)?,
            label,
            session_id: req.session_id,
            session_name,
            created_at: Utc::now().to_rfc3339(),
        }),
    ))
}

/// DELETE /api/admin/subdomains/{label} — remove a custom subdomain.
pub async fn delete_custom_subdomain(
    State(app_state): State<Arc<AppState>>,
    Path(label): Path<String>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<StatusCode, AppError> {
    let admin = require_admin(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;

    use crate::schema::custom_subdomains as cs;
    let deleted = diesel::delete(cs::table.filter(cs::label.eq(&label))).execute(&mut conn)?;
    if deleted == 0 {
        return Err(AppError::NotFound("subdomain"));
    }
    info!("Admin {} removed custom subdomain '{}'", admin.email, label);
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/admin/forwards — every session with an active forward, for the
/// admin subdomain-assignment picker.
pub async fn list_admin_forwards(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<AdminForwardsResponse>, AppError> {
    require_admin(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;

    use crate::schema::{forward_subdomains as fs, session_forwards as sf, sessions, users};
    let rows: Vec<(Uuid, String, String, i32, String)> = sf::table
        .inner_join(sessions::table.on(sessions::id.eq(sf::session_id)))
        .inner_join(users::table.on(users::id.eq(sessions::user_id)))
        .inner_join(fs::table.on(fs::session_id.eq(sf::session_id)))
        .select((
            sf::session_id,
            sessions::session_name,
            users::email,
            sf::port,
            fs::label,
        ))
        .order(sf::created_at.desc())
        .load(&mut conn)?;

    let forwards = rows
        .into_iter()
        .map(|(session_id, session_name, owner_email, port, label)| {
            Ok(AdminForwardInfo {
                session_id,
                session_name,
                owner_email,
                port: port as u16,
                url: label_url(&app_state, &label)?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(AdminForwardsResponse { forwards }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_custom_label_accepts_and_rejects() {
        assert!(validate_custom_label("myapp").is_ok());
        assert!(validate_custom_label("my-app-2").is_ok());
        assert!(validate_custom_label("a").is_ok());

        assert!(validate_custom_label("").is_err());
        assert!(validate_custom_label("-lead").is_err());
        assert!(validate_custom_label("trail-").is_err());
        assert!(validate_custom_label("Upper").is_err());
        assert!(validate_custom_label("has space").is_err());
        assert!(validate_custom_label("under_score").is_err());
        assert!(validate_custom_label("dot.ted").is_err());
        assert!(validate_custom_label(&"a".repeat(64)).is_err());
        // 8-hex is the auto-label namespace.
        assert!(validate_custom_label("a3f9c2e1").is_err());
        // Reserved.
        assert!(validate_custom_label("admin").is_err());
        // 8 chars but not all hex is fine.
        assert!(validate_custom_label("myapp-77").is_ok());
    }
}

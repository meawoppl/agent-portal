use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect},
    Json,
};
use diesel::prelude::*;
use serde::Deserialize;
use shared::api::{
    DeviceCodeRequest, DeviceCodeResponse, DeviceFlowActionResponse, DeviceFlowPollRequest,
};
use shared::DevicePollResponse;
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    auth,
    handlers::proxy_tokens::{issue_proxy_token_with_type, TokenPersist},
    routes, AppState,
};

use shared::protocol::{DEVICE_CODE_EXPIRES_SECS, SESSION_COOKIE_NAME};
use shared::TOKEN_TYPE_MOBILE;

mod api_error;
mod render;
mod state;

#[cfg(test)]
use api_error::DeviceFlowError;
use api_error::{auth_error_to_device_flow, DeviceFlowApiError};
use render::{render_approval_page, render_device_code_form};
use state::{generate_device_code, generate_user_code, VerifyQuery};
pub use state::{DeviceFlowState, DeviceFlowStatus, DeviceFlowStore};

const MOBILE_TOKEN_TTL_DAYS: u32 = 30;

// POST /auth/device/code
pub async fn device_code(
    State(app_state): State<Arc<AppState>>,
    body: Option<Json<DeviceCodeRequest>>,
) -> Result<Json<DeviceCodeResponse>, DeviceFlowApiError> {
    info!(target: "auth_audit", event = "device_code_request");
    let store = app_state
        .device_flow_store
        .as_ref()
        .ok_or_else(DeviceFlowApiError::service_unavailable)?;

    let req = body.map(|b| b.0).unwrap_or_default();
    let hostname = req.hostname;
    let working_directory = req.working_directory;
    let device_code = generate_device_code();
    let user_code = generate_user_code();

    let expires_in = DEVICE_CODE_EXPIRES_SECS;
    let expires_at = std::time::SystemTime::now() + std::time::Duration::from_secs(expires_in);

    let state = DeviceFlowState {
        device_code: device_code.clone(),
        user_code: user_code.clone(),
        user_id: None,
        access_token: None,
        expires_at,
        status: DeviceFlowStatus::Pending,
        hostname: hostname.clone(),
        working_directory: working_directory.clone(),
    };

    let mut store_lock = store.write().await;
    store_lock.insert(device_code.clone(), state);

    let verification_uri = format!("{}/api/auth/device", app_state.public_url);
    info!(
        target: "auth_audit",
        event = "device_code_created",
        user_code = %user_code,
        hostname = ?hostname,
        working_directory = ?working_directory,
    );

    Ok(Json(DeviceCodeResponse {
        device_code,
        user_code,
        verification_uri,
        expires_in,
        interval: 5,
    }))
}

// POST /auth/device/poll
pub async fn device_poll(
    State(app_state): State<Arc<AppState>>,
    Json(req): Json<DeviceFlowPollRequest>,
) -> Result<Json<DevicePollResponse>, DeviceFlowApiError> {
    info!(target: "auth_audit", event = "device_poll_request");
    let store = app_state
        .device_flow_store
        .as_ref()
        .ok_or_else(DeviceFlowApiError::service_unavailable)?;
    let mut store_lock = store.write().await;

    let state = store_lock.get_mut(&req.device_code).ok_or_else(|| {
        warn!(
            target: "auth_audit",
            event = "device_poll_denied",
            reason = "not_found",
        );
        DeviceFlowApiError::not_found("Device code not found or expired")
    })?;

    // Check expiration
    if std::time::SystemTime::now() > state.expires_at {
        state.status = DeviceFlowStatus::Expired;
    }

    match &state.status {
        DeviceFlowStatus::Pending => {
            info!(
                target: "auth_audit",
                event = "device_poll_pending",
                user_code = %state.user_code,
            );
            Ok(Json(DevicePollResponse::Pending))
        }
        DeviceFlowStatus::Complete => {
            let user_id = state
                .user_id
                .ok_or_else(|| DeviceFlowApiError::internal_error("Missing user ID"))?;
            let access_token = state
                .access_token
                .clone()
                .ok_or_else(|| DeviceFlowApiError::internal_error("Missing access token"))?;

            // Fetch user email from database
            use crate::schema::users::dsl::*;
            let mut conn = app_state
                .db_pool
                .get()
                .map_err(|_| DeviceFlowApiError::internal_error("Database connection failed"))?;

            let user = users
                .find(user_id)
                .first::<crate::models::User>(&mut conn)
                .map_err(|_| DeviceFlowApiError::internal_error("User not found"))?;

            info!(
                target: "auth_audit",
                event = "device_poll_complete",
                user_code = %state.user_code,
                user_id = %user_id,
                user_email = %user.email,
            );
            Ok(Json(DevicePollResponse::Complete {
                access_token,
                user_id: user_id.to_string(),
                user_email: user.email,
            }))
        }
        DeviceFlowStatus::Expired => {
            info!(
                target: "auth_audit",
                event = "device_poll_expired",
                user_code = %state.user_code,
            );
            Ok(Json(DevicePollResponse::Expired))
        }
        DeviceFlowStatus::Denied => {
            info!(
                target: "auth_audit",
                event = "device_poll_denied",
                user_code = %state.user_code,
                reason = "user_denied",
            );
            Ok(Json(DevicePollResponse::Denied))
        }
    }
}

// GET /auth/device - Show verification page
pub async fn device_verify_page(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<VerifyQuery>,
) -> impl IntoResponse {
    // If no user_code provided, show a form to enter it.
    let user_code = match query.user_code.as_deref().and_then(normalize_user_code) {
        Some(code) => code,
        None => {
            let html = render_device_code_form(query.user_code.as_deref(), None);
            return axum::response::Html(html).into_response();
        }
    };

    // Check if user code exists and get device info
    let store = match &app_state.device_flow_store {
        Some(s) => s,
        None => return Redirect::temporary(routes::ROOT).into_response(),
    };
    let store_lock = store.read().await;
    let device_info = store_lock
        .values()
        .find(|state| state.user_code == user_code && state.status == DeviceFlowStatus::Pending);

    let (hostname, working_directory) = match device_info {
        Some(state) => (state.hostname.clone(), state.working_directory.clone()),
        None => {
            drop(store_lock);
            // Unknown or expired code: keep the attempted code visible so a
            // mobile user can edit it without retyping from scratch.
            let html = render_device_code_form(
                Some(&user_code),
                Some("That code was not found or has expired."),
            );
            return axum::response::Html(html).into_response();
        }
    };
    drop(store_lock);

    // Check if user is already logged in via session cookie
    if let Some(cookie) = cookies
        .signed(&app_state.cookie_key)
        .get(SESSION_COOKIE_NAME)
    {
        if cookie.value().parse::<Uuid>().is_ok() {
            // User is logged in - show approval page
            let html = render_approval_page(
                &user_code,
                hostname.as_deref(),
                working_directory.as_deref(),
            );
            return axum::response::Html(html).into_response();
        }
    }

    // User not logged in - redirect to device-specific login endpoint
    // This endpoint will handle OAuth and redirect back to the approval page
    Redirect::temporary(&format!(
        "{}?device_user_code={}",
        routes::AUTH_DEVICE_LOGIN,
        user_code
    ))
    .into_response()
}

fn normalize_user_code(input: &str) -> Option<String> {
    let compact: String = input
        .chars()
        .filter(|ch| *ch != '-' && !ch.is_whitespace())
        .map(|ch| ch.to_ascii_uppercase())
        .collect();

    if compact.len() != 6 || !compact.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }

    Some(format!("{}-{}", &compact[..3], &compact[3..]))
}

/// POST /auth/device/approve - Approve device authorization
#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    pub user_code: String,
}

pub async fn device_approve(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<DeviceFlowActionResponse>, DeviceFlowApiError> {
    info!(
        target: "auth_audit",
        event = "device_approve_request",
        user_code = %req.user_code,
    );
    let user_id = auth::extract_user_id(&app_state, None, &cookies).map_err(|e| {
        warn!(
            target: "auth_audit",
            event = "device_approve_denied",
            user_code = %req.user_code,
            reason = "unauthorized",
        );
        auth_error_to_device_flow(e, "approve")
    })?;

    let store = app_state
        .device_flow_store
        .as_ref()
        .ok_or_else(DeviceFlowApiError::service_unavailable)?;

    // Complete the device flow
    complete_device_flow(&app_state, store, &req.user_code, user_id)
        .await
        .map_err(|_| {
            warn!(
                target: "auth_audit",
                event = "device_approve_denied",
                user_code = %req.user_code,
                user_id = %user_id,
                reason = "not_found_or_used",
            );
            DeviceFlowApiError::not_found("Device code not found or already used")
        })?;

    info!(
        "Device flow approved for user_code: {}, user: {}",
        req.user_code, user_id
    );
    info!(
        target: "auth_audit",
        event = "device_approve_success",
        user_code = %req.user_code,
        user_id = %user_id,
    );

    Ok(Json(DeviceFlowActionResponse {
        success: true,
        message: "Device authorized successfully".to_string(),
    }))
}

/// POST /auth/device/deny - Deny device authorization
pub async fn device_deny(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<DeviceFlowActionResponse>, DeviceFlowApiError> {
    info!(
        target: "auth_audit",
        event = "device_deny_request",
        user_code = %req.user_code,
    );
    let user_id = auth::extract_user_id(&app_state, None, &cookies).map_err(|e| {
        warn!(
            target: "auth_audit",
            event = "device_deny_denied",
            user_code = %req.user_code,
            reason = "unauthorized",
        );
        auth_error_to_device_flow(e, "deny")
    })?;

    let store = app_state
        .device_flow_store
        .as_ref()
        .ok_or_else(DeviceFlowApiError::service_unavailable)?;

    // Mark the device flow as denied
    let mut store_lock = store.write().await;
    if let Some(state) = store_lock
        .values_mut()
        .find(|s| s.user_code == req.user_code && s.status == DeviceFlowStatus::Pending)
    {
        state.status = DeviceFlowStatus::Denied;
        info!("Device flow denied for user_code: {}", req.user_code);
        info!(
            target: "auth_audit",
            event = "device_deny_success",
            user_code = %req.user_code,
            user_id = %user_id,
        );
    } else {
        warn!(
            target: "auth_audit",
            event = "device_deny_missing",
            user_code = %req.user_code,
            user_id = %user_id,
        );
    }

    Ok(Json(DeviceFlowActionResponse {
        success: true,
        message: "Device authorization denied".to_string(),
    }))
}

// Called after OAuth success to complete device flow
// Creates a proper JWT token and stores it in the database
pub async fn complete_device_flow(
    app_state: &AppState,
    store: &DeviceFlowStore,
    user_code: &str,
    user_id: Uuid,
) -> Result<(), ()> {
    complete_device_flow_with_expiry(
        app_state,
        store,
        user_code,
        user_id,
        Some(MOBILE_TOKEN_TTL_DAYS),
    )
    .await
}

async fn complete_device_flow_with_expiry(
    app_state: &AppState,
    store: &DeviceFlowStore,
    user_code: &str,
    user_id: Uuid,
    expires_in_days: Option<u32>,
) -> Result<(), ()> {
    let mut conn = app_state.db_pool.get().map_err(|e| {
        error!("Failed to get database connection: {}", e);
    })?;

    // Device-flow credentials are mobile bearer tokens with a fixed lifetime;
    // B2 adds the refresh endpoint so native shells can rotate before expiry.
    let name = format!(
        "Device auth {}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M")
    );
    let issued = issue_proxy_token_with_type(
        &mut conn,
        app_state.jwt_secret.as_bytes(),
        user_id,
        TokenPersist::Create { name: &name },
        expires_in_days,
        TOKEN_TYPE_MOBILE,
    )
    .map_err(|e| {
        error!("Failed to issue device token: {:?}", e);
    })?;

    // Now update the in-memory store with the JWT token
    let mut store_lock = store.write().await;

    // Find the device flow by user_code
    if let Some(state) = store_lock
        .values_mut()
        .find(|s| s.user_code == user_code && s.status == DeviceFlowStatus::Pending)
    {
        state.user_id = Some(user_id);
        state.access_token = Some(issued.token);
        state.status = DeviceFlowStatus::Complete;
        info!(
            "Device flow completed for user_code: {}, user: {}",
            user_code, issued.user_email
        );
        info!(
            target: "auth_audit",
            event = "device_token_issued",
            user_code = %user_code,
            user_id = %user_id,
            user_email = %issued.user_email,
        );
        Ok(())
    } else {
        error!("Device flow state not found for user_code: {}", user_code);
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::state::{generate_device_code_with, generate_user_code_with};
    use super::*;

    #[test]
    fn test_user_code_format() {
        // Generate multiple codes and verify format
        for _ in 0..100 {
            let code = generate_user_code();

            // Should be 7 characters: XXX-XXX
            assert_eq!(code.len(), 7, "User code should be 7 characters: {}", code);

            // Should have dash in the middle
            assert_eq!(
                &code[3..4],
                "-",
                "User code should have dash at position 3: {}",
                code
            );

            // All characters (except dash) should be uppercase alphanumeric
            for (i, c) in code.chars().enumerate() {
                if i == 3 {
                    continue; // Skip the dash
                }
                assert!(
                    c.is_ascii_uppercase() || c.is_ascii_digit(),
                    "Character at position {} should be uppercase alphanumeric: {} in {}",
                    i,
                    c,
                    code
                );
            }
        }
    }

    #[test]
    fn test_user_code_uniqueness() {
        use rand::{rngs::StdRng, SeedableRng};

        // Drive generation from a fixed-seed RNG so this is deterministic: the
        // 1000-code run either always passes or always fails for a given seed,
        // making a collision a real, reproducible regression rather than a
        // birthday-paradox dice roll on `thread_rng()` (see #1133).
        let mut rng = StdRng::seed_from_u64(0xC0DE_1133);
        let mut codes = std::collections::HashSet::new();

        for _ in 0..1000 {
            let code = generate_user_code_with(&mut rng);
            assert!(
                codes.insert(code.clone()),
                "User code collision detected: {}",
                code
            );
        }
    }

    #[test]
    fn test_device_code_format() {
        // Generate multiple codes and verify format
        for _ in 0..100 {
            let code = generate_device_code();

            // Should be 32 alphanumeric characters
            assert_eq!(
                code.len(),
                32,
                "Device code should be 32 characters: {}",
                code
            );

            // All characters should be alphanumeric
            for c in code.chars() {
                assert!(
                    c.is_ascii_alphanumeric(),
                    "All device code characters should be alphanumeric: {} in {}",
                    c,
                    code
                );
            }
        }
    }

    #[test]
    fn test_device_code_uniqueness() {
        use rand::{rngs::StdRng, SeedableRng};

        // Deterministic, like the user-code uniqueness test (#1133). The 32-char
        // device-code space makes collisions astronomically unlikely regardless,
        // but seeding keeps the test reproducible rather than chance-based.
        let mut rng = StdRng::seed_from_u64(0xDEAD_1133);
        let mut codes = std::collections::HashSet::new();

        for _ in 0..1000 {
            let code = generate_device_code_with(&mut rng);
            assert!(
                codes.insert(code.clone()),
                "Device code collision detected: {}",
                code
            );
        }
    }

    #[test]
    fn test_device_flow_state_transitions() {
        // Test that DeviceFlowStatus can represent all states
        let pending = DeviceFlowStatus::Pending;
        let complete = DeviceFlowStatus::Complete;
        let expired = DeviceFlowStatus::Expired;
        let denied = DeviceFlowStatus::Denied;

        assert_eq!(pending, DeviceFlowStatus::Pending);
        assert_eq!(complete, DeviceFlowStatus::Complete);
        assert_eq!(expired, DeviceFlowStatus::Expired);
        assert_eq!(denied, DeviceFlowStatus::Denied);

        // Different states should not be equal
        assert_ne!(pending, complete);
        assert_ne!(pending, expired);
        assert_ne!(pending, denied);
        assert_ne!(complete, expired);
        assert_ne!(complete, denied);
        assert_ne!(expired, denied);
    }

    #[test]
    fn test_device_flow_state_creation() {
        let device_code = generate_device_code();
        let user_code = generate_user_code();
        let expires_in = 300u64;
        let expires_at = std::time::SystemTime::now() + std::time::Duration::from_secs(expires_in);

        let state = DeviceFlowState {
            device_code: device_code.clone(),
            user_code: user_code.clone(),
            user_id: None,
            access_token: None,
            expires_at,
            status: DeviceFlowStatus::Pending,
            hostname: Some("test-host".to_string()),
            working_directory: Some("/home/user/project".to_string()),
        };

        assert_eq!(state.device_code, device_code);
        assert_eq!(state.user_code, user_code);
        assert!(state.user_id.is_none());
        assert!(state.access_token.is_none());
        assert_eq!(state.status, DeviceFlowStatus::Pending);
        assert_eq!(state.hostname, Some("test-host".to_string()));
        assert_eq!(
            state.working_directory,
            Some("/home/user/project".to_string())
        );
    }

    #[tokio::test]
    async fn test_device_flow_store_operations() {
        let store = DeviceFlowStore::default();

        // Create a flow state
        let device_code = generate_device_code();
        let user_code = generate_user_code();
        let expires_at = std::time::SystemTime::now() + std::time::Duration::from_secs(300);

        let state = DeviceFlowState {
            device_code: device_code.clone(),
            user_code: user_code.clone(),
            user_id: None,
            access_token: None,
            expires_at,
            status: DeviceFlowStatus::Pending,
            hostname: Some("test-host".to_string()),
            working_directory: Some("/test/dir".to_string()),
        };

        // Insert into store
        {
            let mut store_lock = store.write().await;
            store_lock.insert(device_code.clone(), state);
        }

        // Verify we can retrieve it
        {
            let store_lock = store.read().await;
            let retrieved = store_lock.get(&device_code);
            assert!(retrieved.is_some());
            let retrieved = retrieved.unwrap();
            assert_eq!(retrieved.user_code, user_code);
            assert_eq!(retrieved.status, DeviceFlowStatus::Pending);
        }

        // Verify we can find by user_code
        {
            let store_lock = store.read().await;
            let found = store_lock
                .values()
                .find(|s| s.user_code == user_code && s.status == DeviceFlowStatus::Pending);
            assert!(found.is_some());
        }

        // Update status to complete
        {
            let mut store_lock = store.write().await;
            if let Some(state) = store_lock.get_mut(&device_code) {
                state.status = DeviceFlowStatus::Complete;
                state.user_id = Some(Uuid::new_v4());
                state.access_token = Some("test-token".to_string());
            }
        }

        // Verify updated state
        {
            let store_lock = store.read().await;
            let retrieved = store_lock.get(&device_code).unwrap();
            assert_eq!(retrieved.status, DeviceFlowStatus::Complete);
            assert!(retrieved.user_id.is_some());
            assert!(retrieved.access_token.is_some());
        }
    }

    #[test]
    fn test_poll_response_serialization() {
        // Test Pending
        let pending = DevicePollResponse::Pending;
        let json = serde_json::to_string(&pending).unwrap();
        assert!(json.contains("\"status\":\"pending\""));

        // Test Complete
        let complete = DevicePollResponse::Complete {
            access_token: "test-token".to_string(),
            user_id: "test-user-id".to_string(),
            user_email: "test@example.com".to_string(),
        };
        let json = serde_json::to_string(&complete).unwrap();
        assert!(json.contains("\"status\":\"complete\""));
        assert!(json.contains("\"access_token\":\"test-token\""));
        assert!(json.contains("\"user_id\":\"test-user-id\""));
        assert!(json.contains("\"user_email\":\"test@example.com\""));

        // Test Expired
        let expired = DevicePollResponse::Expired;
        let json = serde_json::to_string(&expired).unwrap();
        assert!(json.contains("\"status\":\"expired\""));

        // Test Denied
        let denied = DevicePollResponse::Denied;
        let json = serde_json::to_string(&denied).unwrap();
        assert!(json.contains("\"status\":\"denied\""));
    }

    #[test]
    fn test_device_code_response_serialization() {
        let response = DeviceCodeResponse {
            device_code: "abc123".to_string(),
            user_code: "ABC-DEF".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            expires_in: 300,
            interval: 5,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"device_code\":\"abc123\""));
        assert!(json.contains("\"user_code\":\"ABC-DEF\""));
        assert!(json.contains("\"verification_uri\":\"https://example.com/device\""));
        assert!(json.contains("\"expires_in\":300"));
        assert!(json.contains("\"interval\":5"));

        // Verify it can be deserialized back
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["device_code"], "abc123");
        assert_eq!(parsed["user_code"], "ABC-DEF");
    }

    #[test]
    fn test_device_flow_error_serialization() {
        let error = DeviceFlowError {
            error: "not_found".to_string(),
            message: "Device code not found".to_string(),
        };

        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"error\":\"not_found\""));
        assert!(json.contains("\"message\":\"Device code not found\""));
    }

    #[test]
    fn test_verify_query_deserialization() {
        // With user_code
        let json = r#"{"user_code": "ABC-123"}"#;
        let query: VerifyQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.user_code, Some("ABC-123".to_string()));

        // Without user_code (should use None)
        let json = r#"{}"#;
        let query: VerifyQuery = serde_json::from_str(json).unwrap();
        assert!(query.user_code.is_none());
    }

    #[test]
    fn test_poll_request_deserialization() {
        let json = r#"{"device_code": "abc123def456"}"#;
        let request: DeviceFlowPollRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.device_code, "abc123def456");
    }

    #[tokio::test]
    async fn test_device_flow_expiration() {
        let store = DeviceFlowStore::default();

        // Create a flow state that's already expired
        let device_code = generate_device_code();
        let user_code = generate_user_code();
        let expires_at = std::time::SystemTime::now() - std::time::Duration::from_secs(10); // Already expired

        let state = DeviceFlowState {
            device_code: device_code.clone(),
            user_code: user_code.clone(),
            user_id: None,
            access_token: None,
            expires_at,
            status: DeviceFlowStatus::Pending,
            hostname: None,
            working_directory: None,
        };

        // Insert into store
        {
            let mut store_lock = store.write().await;
            store_lock.insert(device_code.clone(), state);
        }

        // Check that expiration detection works
        {
            let store_lock = store.read().await;
            let state = store_lock.get(&device_code).unwrap();

            // The actual expiration check happens in device_poll handler
            let is_expired = std::time::SystemTime::now() > state.expires_at;
            assert!(is_expired, "State should be detected as expired");
        }
    }

    #[test]
    fn test_device_code_form_html_content() {
        // Verify the HTML form contains expected elements
        let html = render_device_code_form(None, None);

        assert!(
            html.contains("Device Authentication"),
            "Should contain title"
        );
        assert!(html.contains("user_code"), "Should contain user_code input");
        assert!(html.contains("<form"), "Should contain form element");
        assert!(
            html.contains("/api/auth/device"),
            "Should submit to device endpoint"
        );
        assert!(html.contains("XXX-XXX"), "Should show expected format");
        assert!(
            html.contains("pattern="),
            "Should have input validation pattern"
        );
    }

    #[test]
    fn device_code_form_prefills_and_escapes_attempted_code() {
        let html = render_device_code_form(
            Some(r#"ABC-123" autofocus onfocus="alert(1)"#),
            Some(r#"Bad <code>"#),
        );

        assert!(html.contains("ABC-123&quot;"));
        assert!(!html.contains(r#"value="ABC-123" autofocus"#));
        assert!(html.contains("Bad &lt;code&gt;"));
    }

    #[test]
    fn normalize_user_code_accepts_mobile_friendly_variants() {
        assert_eq!(normalize_user_code("abc123").as_deref(), Some("ABC-123"));
        assert_eq!(normalize_user_code("abc-123").as_deref(), Some("ABC-123"));
        assert_eq!(normalize_user_code(" abc 123 ").as_deref(), Some("ABC-123"));
        assert_eq!(normalize_user_code("abc12").as_deref(), None);
        assert_eq!(normalize_user_code("abc-12!").as_deref(), None);
    }

    #[test]
    fn approval_page_escapes_device_metadata() {
        let html = render_approval_page(
            "ABC-123",
            Some(r#"<img src=x onerror="alert(1)">"#),
            Some(r#"repo<script>alert('x')"#),
        );

        assert!(!html.contains("<img src=x"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;img src=x onerror=&quot;alert(1)&quot;&gt;"));
        assert!(html.contains("repo&lt;script&gt;alert(&#39;x&#39;)"));
    }

    #[test]
    fn approval_page_serializes_user_code_as_javascript_string() {
        let html = render_approval_page(r#"ABC-123";alert(1)//"#, None, None);

        assert!(html.contains(r#"const userCode = "ABC-123\";alert(1)//";"#));
        assert!(!html.contains(r#"const userCode = "ABC-123";alert(1)//";"#));
    }

    #[test]
    fn escape_html_text_escapes_text_node_metacharacters() {
        assert_eq!(
            render::escape_html_text(r#"<>&"'"#),
            "&lt;&gt;&amp;&quot;&#39;"
        );
    }
}

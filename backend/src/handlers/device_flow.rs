use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect},
    Json,
};
use diesel::prelude::*;
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use shared::api::{DeviceCodeRequest, DeviceFlowActionResponse, DeviceFlowPollRequest};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_cookies::Cookies;
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    auth,
    errors::AppError,
    jwt::{create_proxy_token, hash_token},
    models::NewProxyAuthToken,
    schema::proxy_auth_tokens,
    AppState,
};

use shared::protocol::{DEVICE_CODE_EXPIRES_SECS, SESSION_COOKIE_NAME};

/// Error response for device flow endpoints
#[derive(Debug, Serialize)]
pub struct DeviceFlowError {
    pub error: String,
    pub message: String,
}

/// Device flow API error that returns JSON
pub struct DeviceFlowApiError {
    status: StatusCode,
    error: String,
    message: String,
}

impl DeviceFlowApiError {
    fn service_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error: "service_unavailable".to_string(),
            message: "Device flow authentication is not available. Server may be in dev mode or OAuth is not configured.".to_string(),
        }
    }

    fn not_found(msg: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: "not_found".to_string(),
            message: msg.to_string(),
        }
    }

    fn internal_error(msg: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "internal_error".to_string(),
            message: msg.to_string(),
        }
    }
}

impl IntoResponse for DeviceFlowApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(DeviceFlowError {
                error: self.error,
                message: self.message,
            }),
        )
            .into_response()
    }
}

fn auth_error_to_device_flow(error: AppError, action: &str) -> DeviceFlowApiError {
    match error {
        AppError::Forbidden => DeviceFlowApiError {
            status: StatusCode::FORBIDDEN,
            error: "forbidden".to_string(),
            message: format!("You are not allowed to {} device authorization", action),
        },
        AppError::DbPool | AppError::DbQuery(_) => {
            DeviceFlowApiError::internal_error("Database connection failed")
        }
        _ => DeviceFlowApiError {
            status: StatusCode::UNAUTHORIZED,
            error: "unauthorized".to_string(),
            message: format!("You must be logged in to {} device authorization", action),
        },
    }
}

// In-memory store for device flow state
// In production, use Redis or database
pub type DeviceFlowStore = Arc<RwLock<HashMap<String, DeviceFlowState>>>;

#[derive(Debug, Clone)]
pub struct DeviceFlowState {
    pub device_code: String,
    pub user_code: String,
    pub user_id: Option<Uuid>,
    pub access_token: Option<String>,
    pub expires_at: std::time::SystemTime,
    pub status: DeviceFlowStatus,
    /// Hostname of the machine requesting authorization
    pub hostname: Option<String>,
    /// Working directory / repository path
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceFlowStatus {
    Pending,
    Complete,
    Expired,
    Denied,
}

#[derive(Debug, Serialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status")]
pub enum PollResponse {
    #[serde(rename = "pending")]
    Pending,

    #[serde(rename = "complete")]
    Complete {
        access_token: String,
        user_id: String,
        user_email: String,
    },

    #[serde(rename = "expired")]
    Expired,

    #[serde(rename = "denied")]
    Denied,
}

#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    pub user_code: Option<String>,
}

fn generate_user_code() -> String {
    let chars: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(|c| c as char)
        .collect::<String>()
        .to_uppercase();

    // Format as XXX-XXX for readability
    format!("{}-{}", &chars[0..3], &chars[3..6])
}

fn generate_device_code() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(|c| c as char)
        .collect()
}

// POST /auth/device/code
pub async fn device_code(
    State(app_state): State<Arc<AppState>>,
    body: Option<Json<DeviceCodeRequest>>,
) -> Result<Json<DeviceCodeResponse>, DeviceFlowApiError> {
    let store = app_state
        .device_flow_store
        .as_ref()
        .ok_or_else(DeviceFlowApiError::service_unavailable)?;

    let req = body.map(|b| b.0).unwrap_or_default();
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
        hostname: req.hostname,
        working_directory: req.working_directory,
    };

    let mut store_lock = store.write().await;
    store_lock.insert(device_code.clone(), state);

    let verification_uri = format!("{}/api/auth/device", app_state.public_url);

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
) -> Result<Json<PollResponse>, DeviceFlowApiError> {
    let store = app_state
        .device_flow_store
        .as_ref()
        .ok_or_else(DeviceFlowApiError::service_unavailable)?;
    let mut store_lock = store.write().await;

    let state = store_lock
        .get_mut(&req.device_code)
        .ok_or_else(|| DeviceFlowApiError::not_found("Device code not found or expired"))?;

    // Check expiration
    if std::time::SystemTime::now() > state.expires_at {
        state.status = DeviceFlowStatus::Expired;
    }

    match &state.status {
        DeviceFlowStatus::Pending => Ok(Json(PollResponse::Pending)),
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

            Ok(Json(PollResponse::Complete {
                access_token,
                user_id: user_id.to_string(),
                user_email: user.email,
            }))
        }
        DeviceFlowStatus::Expired => Ok(Json(PollResponse::Expired)),
        DeviceFlowStatus::Denied => Ok(Json(PollResponse::Denied)),
    }
}

// GET /auth/device - Show verification page
pub async fn device_verify_page(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<VerifyQuery>,
) -> impl IntoResponse {
    // If no user_code provided, show a form to enter it
    let user_code = match query.user_code {
        Some(code) => code,
        None => {
            return axum::response::Html(DEVICE_CODE_FORM_HTML.to_string()).into_response();
        }
    };

    // Check if user code exists and get device info
    let store = match &app_state.device_flow_store {
        Some(s) => s,
        None => return Redirect::temporary("/").into_response(),
    };
    let store_lock = store.read().await;
    let device_info = store_lock
        .values()
        .find(|state| state.user_code == user_code && state.status == DeviceFlowStatus::Pending);

    let (hostname, working_directory) = match device_info {
        Some(state) => (state.hostname.clone(), state.working_directory.clone()),
        None => {
            drop(store_lock);
            return Redirect::temporary("/api/auth/device/error?message=Invalid+or+expired+code")
                .into_response();
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
        "/api/auth/device-login?device_user_code={}",
        user_code
    ))
    .into_response()
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
    let user_id = auth::extract_user_id(&app_state, &cookies)
        .map_err(|e| auth_error_to_device_flow(e, "approve"))?;

    let store = app_state
        .device_flow_store
        .as_ref()
        .ok_or_else(DeviceFlowApiError::service_unavailable)?;

    // Complete the device flow
    complete_device_flow(&app_state, store, &req.user_code, user_id)
        .await
        .map_err(|_| DeviceFlowApiError::not_found("Device code not found or already used"))?;

    info!(
        "Device flow approved for user_code: {}, user: {}",
        req.user_code, user_id
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
    auth::extract_user_id(&app_state, &cookies)
        .map_err(|e| auth_error_to_device_flow(e, "deny"))?;

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
    }

    Ok(Json(DeviceFlowActionResponse {
        success: true,
        message: "Device authorization denied".to_string(),
    }))
}

fn render_approval_page(
    user_code: &str,
    hostname: Option<&str>,
    working_directory: Option<&str>,
) -> String {
    let hostname_display = hostname.unwrap_or("Unknown device");
    let working_dir_display = working_directory
        .map(|wd| {
            // Extract just the last component (likely repo name)
            std::path::Path::new(wd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(wd)
        })
        .unwrap_or("Unknown directory");

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Approve Device - Agent Portal</title>
    <style>
        :root {{
            --bg-dark: #1a1b26;
            --bg-darker: #16161e;
            --text-primary: #c0caf5;
            --text-secondary: #7f849c;
            --accent: #7aa2f7;
            --accent-hover: #9eb3ff;
            --border: #292e42;
            --success: #9ece6a;
            --error: #f7768e;
            --warning: #e0af68;
        }}
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            background: var(--bg-dark);
            color: var(--text-primary);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
        }}
        .container {{
            background: var(--bg-darker);
            border: 1px solid var(--border);
            border-radius: 12px;
            padding: 2rem;
            max-width: 450px;
            width: 90%;
            text-align: center;
        }}
        h1 {{
            font-size: 1.5rem;
            margin-bottom: 0.5rem;
            color: var(--warning);
        }}
        .subtitle {{
            color: var(--text-secondary);
            margin-bottom: 1.5rem;
            font-size: 0.9rem;
        }}
        .device-info {{
            background: var(--bg-dark);
            border: 1px solid var(--border);
            border-radius: 8px;
            padding: 1rem;
            margin-bottom: 1.5rem;
            text-align: left;
        }}
        .device-info .label {{
            color: var(--text-secondary);
            font-size: 0.75rem;
            text-transform: uppercase;
            letter-spacing: 0.05em;
            margin-bottom: 0.25rem;
        }}
        .device-info .value {{
            color: var(--text-primary);
            font-family: 'Courier New', monospace;
            font-size: 0.95rem;
            margin-bottom: 0.75rem;
            word-break: break-all;
        }}
        .device-info .value:last-child {{
            margin-bottom: 0;
        }}
        .code-display {{
            background: var(--bg-dark);
            border: 2px solid var(--accent);
            border-radius: 8px;
            padding: 0.75rem;
            font-family: 'Courier New', monospace;
            font-size: 1.25rem;
            letter-spacing: 0.2rem;
            color: var(--accent);
            margin-bottom: 1.5rem;
        }}
        .buttons {{
            display: flex;
            gap: 1rem;
        }}
        button {{
            flex: 1;
            padding: 0.75rem 1.5rem;
            font-size: 1rem;
            border: none;
            border-radius: 8px;
            cursor: pointer;
            font-weight: 600;
            transition: all 0.2s;
        }}
        .approve {{
            background: var(--success);
            color: var(--bg-dark);
        }}
        .approve:hover {{
            filter: brightness(1.1);
        }}
        .deny {{
            background: transparent;
            border: 1px solid var(--error);
            color: var(--error);
        }}
        .deny:hover {{
            background: var(--error);
            color: var(--bg-dark);
        }}
        .warning {{
            color: var(--text-secondary);
            font-size: 0.8rem;
            margin-top: 1rem;
        }}
        .result {{
            display: none;
            padding: 1rem;
            border-radius: 8px;
            margin-top: 1rem;
        }}
        .result.success {{
            background: rgba(158, 206, 106, 0.1);
            border: 1px solid var(--success);
            color: var(--success);
        }}
        .result.error {{
            background: rgba(247, 118, 142, 0.1);
            border: 1px solid var(--error);
            color: var(--error);
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>⚠️ Authorize Device?</h1>
        <p class="subtitle">A device is requesting access to your Claude Code sessions</p>

        <div class="device-info">
            <div class="label">Machine</div>
            <div class="value">{hostname_display}</div>
            <div class="label">Directory</div>
            <div class="value">{working_dir_display}</div>
        </div>

        <div class="code-display">{user_code}</div>

        <div class="buttons">
            <button class="deny" onclick="denyDevice()">Deny</button>
            <button class="approve" onclick="approveDevice()">Approve</button>
        </div>

        <div id="result" class="result"></div>

        <p class="warning">Only approve if you initiated this request from your terminal.</p>
    </div>

    <script>
        const userCode = "{user_code}";

        async function approveDevice() {{
            try {{
                const response = await fetch('/api/auth/device/approve', {{
                    method: 'POST',
                    headers: {{ 'Content-Type': 'application/json' }},
                    body: JSON.stringify({{ user_code: userCode }})
                }});
                const data = await response.json();
                if (response.ok) {{
                    showResult('success', 'Device authorized! You can close this page or return to the dashboard.');
                    setTimeout(() => window.location.href = '/dashboard', 2000);
                }} else {{
                    showResult('error', data.message || 'Failed to authorize device');
                }}
            }} catch (e) {{
                showResult('error', 'Network error: ' + e.message);
            }}
        }}

        async function denyDevice() {{
            try {{
                const response = await fetch('/api/auth/device/deny', {{
                    method: 'POST',
                    headers: {{ 'Content-Type': 'application/json' }},
                    body: JSON.stringify({{ user_code: userCode }})
                }});
                const data = await response.json();
                if (response.ok) {{
                    showResult('error', 'Device authorization denied.');
                    setTimeout(() => window.location.href = '/', 1500);
                }} else {{
                    showResult('error', data.message || 'Failed to deny device');
                }}
            }} catch (e) {{
                showResult('error', 'Network error: ' + e.message);
            }}
        }}

        function showResult(type, message) {{
            const result = document.getElementById('result');
            result.className = 'result ' + type;
            result.textContent = message;
            result.style.display = 'block';
            document.querySelector('.buttons').style.display = 'none';
        }}
    </script>
</body>
</html>"#
    )
}

const DEVICE_CODE_FORM_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Device Authentication - Agent Portal</title>
    <style>
        :root {
            --bg-dark: #1a1b26;
            --bg-darker: #16161e;
            --text-primary: #c0caf5;
            --text-secondary: #7f849c;
            --accent: #7aa2f7;
            --accent-hover: #9eb3ff;
            --border: #292e42;
            --success: #9ece6a;
            --error: #f7768e;
        }
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }
        body {
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            background: var(--bg-dark);
            color: var(--text-primary);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
        }
        .container {
            background: var(--bg-darker);
            border: 1px solid var(--border);
            border-radius: 12px;
            padding: 2rem;
            max-width: 400px;
            width: 90%;
            text-align: center;
        }
        h1 {
            font-size: 1.5rem;
            margin-bottom: 0.5rem;
            color: var(--accent);
        }
        p {
            color: var(--text-secondary);
            margin-bottom: 1.5rem;
            font-size: 0.9rem;
        }
        .code-input {
            width: 100%;
            padding: 1rem;
            font-size: 1.5rem;
            text-align: center;
            background: var(--bg-dark);
            border: 2px solid var(--border);
            border-radius: 8px;
            color: var(--text-primary);
            font-family: 'Courier New', monospace;
            letter-spacing: 0.25rem;
            text-transform: uppercase;
            margin-bottom: 1rem;
        }
        .code-input:focus {
            outline: none;
            border-color: var(--accent);
        }
        .code-input::placeholder {
            color: var(--text-secondary);
            letter-spacing: normal;
            text-transform: none;
        }
        button {
            width: 100%;
            padding: 0.75rem 1.5rem;
            font-size: 1rem;
            background: var(--accent);
            color: var(--bg-dark);
            border: none;
            border-radius: 8px;
            cursor: pointer;
            font-weight: 600;
            transition: background 0.2s;
        }
        button:hover {
            background: var(--accent-hover);
        }
        .hint {
            margin-top: 1rem;
            font-size: 0.8rem;
            color: var(--text-secondary);
        }
    </style>
</head>
<body>
    <div class="container">
        <h1>Device Authentication</h1>
        <p>Enter the code displayed in your terminal to authenticate this device.</p>
        <form action="/api/auth/device" method="get">
            <input
                type="text"
                name="user_code"
                class="code-input"
                placeholder="XXX-XXX"
                pattern="[A-Za-z0-9]{3}-?[A-Za-z0-9]{3}"
                maxlength="7"
                required
                autofocus
            >
            <button type="submit">Continue</button>
        </form>
        <p class="hint">The code is shown in your terminal after running <code>claude-portal</code></p>
    </div>
</body>
</html>
"#;

// Called after OAuth success to complete device flow
// Creates a proper JWT token and stores it in the database
pub async fn complete_device_flow(
    app_state: &AppState,
    store: &DeviceFlowStore,
    user_code: &str,
    user_id: Uuid,
) -> Result<(), ()> {
    // First, get user email from database (needed for JWT claims)
    let mut conn = app_state.db_pool.get().map_err(|e| {
        error!("Failed to get database connection: {}", e);
    })?;

    use crate::schema::users;
    let user: crate::models::User = users::table.find(user_id).first(&mut conn).map_err(|e| {
        error!("Failed to find user: {}", e);
    })?;

    // Generate token ID and create JWT
    let token_id = Uuid::new_v4();
    let expires_in_days: u32 = 30; // Device flow tokens valid for 30 days
    let jwt_secret = app_state.jwt_secret.as_bytes();

    let token = create_proxy_token(jwt_secret, token_id, user_id, &user.email, expires_in_days)
        .map_err(|e| {
            error!("Failed to create JWT: {}", e);
        })?;

    // Store token hash in database
    let token_hash = hash_token(&token);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(expires_in_days as i64);

    let new_token = NewProxyAuthToken {
        user_id,
        name: format!(
            "Device auth {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M")
        ),
        token_hash,
        expires_at: expires_at.naive_utc(),
    };

    diesel::insert_into(proxy_auth_tokens::table)
        .values(&new_token)
        .execute(&mut conn)
        .map_err(|e| {
            error!("Failed to save token to database: {}", e);
        })?;

    // Now update the in-memory store with the JWT token
    let mut store_lock = store.write().await;

    // Find the device flow by user_code
    if let Some(state) = store_lock
        .values_mut()
        .find(|s| s.user_code == user_code && s.status == DeviceFlowStatus::Pending)
    {
        state.user_id = Some(user_id);
        state.access_token = Some(token);
        state.status = DeviceFlowStatus::Complete;
        info!(
            "Device flow completed for user_code: {}, user: {}",
            user_code, user.email
        );
        Ok(())
    } else {
        error!("Device flow state not found for user_code: {}", user_code);
        Err(())
    }
}

#[cfg(test)]
mod tests {
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
        let mut codes = std::collections::HashSet::new();

        // Generate 1000 codes and verify no collisions
        for _ in 0..1000 {
            let code = generate_user_code();
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
        let mut codes = std::collections::HashSet::new();

        // Generate 1000 codes and verify no collisions
        for _ in 0..1000 {
            let code = generate_device_code();
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
        let pending = PollResponse::Pending;
        let json = serde_json::to_string(&pending).unwrap();
        assert!(json.contains("\"status\":\"pending\""));

        // Test Complete
        let complete = PollResponse::Complete {
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
        let expired = PollResponse::Expired;
        let json = serde_json::to_string(&expired).unwrap();
        assert!(json.contains("\"status\":\"expired\""));

        // Test Denied
        let denied = PollResponse::Denied;
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
        assert!(
            DEVICE_CODE_FORM_HTML.contains("Device Authentication"),
            "Should contain title"
        );
        assert!(
            DEVICE_CODE_FORM_HTML.contains("user_code"),
            "Should contain user_code input"
        );
        assert!(
            DEVICE_CODE_FORM_HTML.contains("<form"),
            "Should contain form element"
        );
        assert!(
            DEVICE_CODE_FORM_HTML.contains("/api/auth/device"),
            "Should submit to device endpoint"
        );
        assert!(
            DEVICE_CODE_FORM_HTML.contains("XXX-XXX"),
            "Should show expected format"
        );
        assert!(
            DEVICE_CODE_FORM_HTML.contains("pattern="),
            "Should have input validation pattern"
        );
    }
}

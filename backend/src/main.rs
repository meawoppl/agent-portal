mod db;
mod embedded_assets;
mod handlers;
mod jwt;
mod models;
mod schema;
mod speech;

use crate::db::DbPool;
use crate::handlers::device_flow::DeviceFlowStore;
use axum::{
    routing::{get, post},
    Router,
};
use clap::Parser;
use oauth2::{basic::BasicClient, AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use shared::WsEndpoint;
use std::{env, sync::Arc};
use tower_cookies::{CookieManagerLayer, Key};
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use handlers::websocket::SessionManager;

#[derive(Parser, Debug, Clone)]
#[command(name = "cc-proxy-backend")]
#[command(about = "CC-Proxy backend server")]
struct Args {
    /// Enable development mode (bypasses OAuth, creates test user)
    #[arg(long)]
    dev_mode: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub dev_mode: bool,
    pub db_pool: DbPool,
    pub session_manager: SessionManager,
    pub oauth_basic_client: Option<BasicClient>,
    pub device_flow_store: Option<DeviceFlowStore>,
    pub public_url: String,
    pub cookie_key: Key,
    pub jwt_secret: String,
    pub speech_credentials_path: Option<String>,
    pub app_title: String,
    /// Allowed email domain (e.g., "company.com")
    pub allowed_email_domain: Option<String>,
    /// Allowed email addresses (comma-separated in env var)
    pub allowed_emails: Option<Vec<String>>,
    /// Maximum messages to keep per session (default: 100)
    pub message_retention_count: i64,
    /// Days to retain messages before deletion (default: 30, 0 = disabled)
    pub message_retention_days: u32,
    /// Days to keep sessions before auto-deletion (default: 14, 0 = disabled)
    pub session_max_age_days: u32,
    /// Maximum image size in MB that proxies should inline (default: 10)
    pub max_image_mb: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments
    let args = Args::parse();

    // Initialize tracing with info level by default
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    if args.dev_mode {
        tracing::warn!("🚧 DEV MODE ENABLED - OAuth is bypassed, test user will be used");
    }

    // Load environment variables
    dotenvy::dotenv().ok();

    // Create database pool
    let pool = db::create_pool()?;

    // Run pending migrations automatically
    tracing::info!("Running database migrations...");
    match db::run_migrations(&pool) {
        Ok(applied) => {
            if applied.is_empty() {
                tracing::info!("Database is up to date (no pending migrations)");
            } else {
                for migration in &applied {
                    tracing::info!("Applied migration: {}", migration);
                }
                tracing::info!("Successfully applied {} migration(s)", applied.len());
            }
        }
        Err(e) => {
            tracing::error!("Failed to run database migrations: {}", e);
            return Err(e);
        }
    }

    // Create device flow store
    let device_flow_store = handlers::device_flow::DeviceFlowStore::default();

    // Create OAuth client (skip in dev mode)
    let oauth_basic_client = if !args.dev_mode {
        let client_id =
            ClientId::new(env::var("GOOGLE_CLIENT_ID").expect("GOOGLE_CLIENT_ID must be set"));
        let client_secret = ClientSecret::new(
            env::var("GOOGLE_CLIENT_SECRET").expect("GOOGLE_CLIENT_SECRET must be set"),
        );
        let auth_url = AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())?;
        let token_url = TokenUrl::new("https://oauth2.googleapis.com/token".to_string())?;
        let redirect_uri = RedirectUrl::new(
            env::var("GOOGLE_REDIRECT_URI").expect("GOOGLE_REDIRECT_URI must be set"),
        )?;

        Some(
            BasicClient::new(client_id, Some(client_secret), auth_url, Some(token_url))
                .set_redirect_uri(redirect_uri),
        )
    } else {
        None
    };

    // Create test user in dev mode
    if args.dev_mode {
        use diesel::prelude::*;
        use models::NewUser;
        use schema::users::dsl::*;

        let mut conn = pool.get()?;
        let test_user = users
            .filter(email.eq("testing@testing.local"))
            .first::<models::User>(&mut conn)
            .optional()?;

        if test_user.is_none() {
            let new_user = NewUser {
                google_id: "dev_mode_test_user".to_string(),
                email: "testing@testing.local".to_string(),
                name: Some("Test User".to_string()),
                avatar_url: None,
            };

            diesel::insert_into(users)
                .values(&new_user)
                .execute(&mut conn)?;

            tracing::info!("✓ Created test user: testing@testing.local");
        }
    }

    // Create session manager for WebSocket connections
    let session_manager = SessionManager::new();

    // Cleanup stale sessions on startup (mark all "active" sessions as "disconnected"
    // since they can't be active if we just started)
    {
        use diesel::prelude::*;
        use schema::sessions::dsl::*;
        let mut conn = pool.get()?;
        let updated = diesel::update(sessions.filter(status.eq("active")))
            .set(status.eq("disconnected"))
            .execute(&mut conn)?;
        if updated > 0 {
            tracing::info!(
                "Marked {} stale sessions as disconnected on startup",
                updated
            );
        }
    }

    // Get base URL from env or construct from host/port
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let public_url = env::var("BASE_URL").unwrap_or_else(|_| {
        // Default to localhost for development
        format!("http://localhost:{}", port)
    });

    // Create cookie signing key from SESSION_SECRET or generate random for dev
    let session_secret = env::var("SESSION_SECRET").ok();
    let cookie_key = match &session_secret {
        Some(secret) => {
            let bytes = secret.as_bytes();
            if bytes.len() < 64 {
                tracing::warn!("SESSION_SECRET should be at least 64 bytes, padding with zeros");
                let mut padded = vec![0u8; 64];
                padded[..bytes.len()].copy_from_slice(bytes);
                Key::from(&padded)
            } else {
                Key::from(&bytes[..64])
            }
        }
        None => {
            if args.dev_mode {
                tracing::warn!("No SESSION_SECRET set, using random key (sessions won't persist across restarts)");
                Key::generate()
            } else {
                panic!("SESSION_SECRET must be set in production mode");
            }
        }
    };

    // Google Cloud Speech credentials path
    let speech_credentials_path = env::var("GOOGLE_APPLICATION_CREDENTIALS").ok();
    if speech_credentials_path.is_some() {
        tracing::info!("Google Cloud Speech credentials configured for voice input");
    } else {
        tracing::info!("Voice input disabled - GOOGLE_APPLICATION_CREDENTIALS not set");
    }

    // JWT secret for proxy tokens (uses SESSION_SECRET or generates for dev)
    let jwt_secret = session_secret.unwrap_or_else(|| {
        if args.dev_mode {
            "dev-mode-jwt-secret-not-for-production".to_string()
        } else {
            panic!("SESSION_SECRET must be set in production mode");
        }
    });

    // App title (customizable via environment variable)
    // In dev mode, override with a warning to make it obvious
    let app_title = if args.dev_mode {
        "⚠️ INSECURE DEV MODE ⚠️".to_string()
    } else {
        env::var("APP_TITLE").unwrap_or_else(|_| "Agent Portal".to_string())
    };

    // Email access control (optional)
    let allowed_email_domain = env::var("ALLOWED_EMAIL_DOMAIN").ok();
    let allowed_emails = env::var("ALLOWED_EMAILS").ok().map(|s| {
        s.split(',')
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .collect::<Vec<_>>()
    });

    if allowed_email_domain.is_some() || allowed_emails.is_some() {
        tracing::info!(
            "Email access control enabled: domain={:?}, specific_emails={}",
            allowed_email_domain,
            allowed_emails.as_ref().map(|e| e.len()).unwrap_or(0)
        );
    }

    // Message retention settings
    let message_retention_count: i64 = env::var("MESSAGE_RETENTION_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let message_retention_days: u32 = env::var("MESSAGE_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let session_max_age_days: u32 = env::var("SESSION_MAX_AGE_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(14);

    let max_image_mb: u32 = env::var("PORTAL_MAX_IMAGE_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    tracing::info!(
        "Message retention: max {} messages/session, {} days",
        message_retention_count,
        message_retention_days
    );
    tracing::info!(
        "Session max age: {} days (0 = disabled)",
        session_max_age_days
    );
    tracing::info!("Max image size: {} MB", max_image_mb);

    // Create app state
    let app_state = Arc::new(AppState {
        dev_mode: args.dev_mode,
        db_pool: pool.clone(),
        session_manager: session_manager.clone(),
        oauth_basic_client,
        device_flow_store: Some(device_flow_store.clone()),
        public_url: public_url.clone(),
        cookie_key,
        jwt_secret,
        speech_credentials_path,
        app_title,
        allowed_email_domain,
        allowed_emails,
        message_retention_count,
        message_retention_days,
        session_max_age_days,
        max_image_mb,
    });

    // Setup CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Build our application with routes
    let app = Router::new()
        // Health check endpoint
        .route("/api/health", get(|| async { "OK" }))
        // App configuration (public, no auth required)
        .route("/api/config", get(handlers::config::get_config))
        // Session API routes
        .route("/api/sessions", get(handlers::sessions::list_sessions))
        .route("/api/sessions/:id", get(handlers::sessions::get_session))
        .route(
            "/api/sessions/:id",
            axum::routing::delete(handlers::sessions::delete_session),
        )
        .route(
            "/api/sessions/:id/stop",
            post(handlers::sessions::stop_session),
        )
        // Session member management routes
        .route(
            "/api/sessions/:id/members",
            get(handlers::sessions::list_session_members)
                .post(handlers::sessions::add_session_member),
        )
        .route(
            "/api/sessions/:id/members/:user_id",
            axum::routing::delete(handlers::sessions::remove_session_member)
                .patch(handlers::sessions::update_session_member_role),
        )
        .route(
            "/api/sessions/:id/messages",
            get(handlers::messages::list_messages).post(handlers::messages::create_message),
        )
        // Proxy token management endpoints
        .route(
            "/api/proxy-tokens",
            get(handlers::proxy_tokens::list_tokens_handler)
                .post(handlers::proxy_tokens::create_token_handler),
        )
        .route(
            "/api/proxy-tokens/:id",
            axum::routing::delete(handlers::proxy_tokens::revoke_token_handler),
        )
        // Sound settings
        .route(
            "/api/settings/sound",
            get(handlers::sound_settings::get_sound_settings)
                .put(handlers::sound_settings::save_sound_settings),
        )
        // Auth routes (under /api/auth)
        .route("/api/auth/google", get(handlers::auth::login))
        .route("/api/auth/google/callback", get(handlers::auth::callback))
        .route("/api/auth/me", get(handlers::auth::me))
        .route("/api/auth/logout", get(handlers::auth::logout))
        .route("/api/auth/dev-login", get(handlers::auth::dev_login))
        // Device-specific login endpoint (separate from regular web login)
        .route("/api/auth/device-login", get(handlers::auth::device_login))
        // Device flow endpoints for CLI (under /api/auth)
        .route(
            "/api/auth/device/code",
            post(handlers::device_flow::device_code),
        )
        .route(
            "/api/auth/device/poll",
            post(handlers::device_flow::device_poll),
        )
        .route(
            "/api/auth/device",
            get(handlers::device_flow::device_verify_page),
        )
        .route(
            "/api/auth/device/approve",
            post(handlers::device_flow::device_approve),
        )
        .route(
            "/api/auth/device/deny",
            post(handlers::device_flow::device_deny),
        )
        // WebSocket routes (paths from ws-bridge endpoint definitions)
        .route(
            shared::SessionEndpoint::PATH,
            get(handlers::websocket::handle_session_websocket),
        )
        .route(
            shared::ClientEndpoint::PATH,
            get(handlers::websocket::handle_web_client_websocket),
        )
        .route(
            "/ws/voice/:session_id",
            get(handlers::voice::handle_voice_websocket),
        )
        .route(
            shared::LauncherEndpoint::PATH,
            get(handlers::websocket::handle_launcher_websocket),
        )
        // Launcher API routes
        .route("/api/launchers", get(handlers::launchers::list_launchers))
        .route(
            "/api/launchers/:launcher_id/directories",
            get(handlers::launchers::list_directories),
        )
        .route("/api/launch", post(handlers::launchers::launch_session))
        // Download routes for proxy binary and install script
        .route(
            "/api/download/install.sh",
            get(handlers::downloads::install_script),
        )
        .route(
            "/api/download/proxy",
            get(handlers::downloads::proxy_binary).head(handlers::downloads::proxy_binary),
        )
        // Admin dashboard routes (admin-only)
        .route("/api/admin/stats", get(handlers::admin::get_stats))
        .route("/api/admin/users", get(handlers::admin::list_users))
        .route(
            "/api/admin/users/:id",
            axum::routing::patch(handlers::admin::update_user),
        )
        .route("/api/admin/sessions", get(handlers::admin::list_sessions))
        .route(
            "/api/admin/sessions/:id",
            axum::routing::delete(handlers::admin::delete_session),
        )
        // Raw message logging (for debugging unrecognized message types)
        .route("/api/raw-messages", post(handlers::admin::log_raw_message))
        .route(
            "/api/admin/raw-messages",
            get(handlers::admin::list_raw_messages),
        )
        .route(
            "/api/admin/raw-messages/:id",
            get(handlers::admin::get_raw_message).delete(handlers::admin::delete_raw_message),
        )
        // Add single unified state
        .with_state(app_state.clone())
        // Serve embedded frontend assets with SPA fallback
        .fallback(axum::routing::get(embedded_assets::serve_embedded_frontend));

    tracing::info!("Serving embedded frontend assets");

    // Add CORS and cookie management
    let app = app.layer(CookieManagerLayer::new()).layer(cors);

    // Spawn background task to broadcast user spend updates
    {
        let app_state = app_state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                broadcast_user_spend_updates(&app_state).await;
            }
        });
        tracing::info!("Started user spend broadcast task (every 5 seconds)");
    }

    // Spawn background task for message retention cleanup (runs every 60 seconds)
    {
        let app_state = app_state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                run_retention_cleanup(&app_state).await;
            }
        });
        tracing::info!("Started message retention task (every 60 seconds)");
    }

    // Spawn background task for session age cleanup (runs every hour)
    if app_state.session_max_age_days > 0 {
        let max_age_days = app_state.session_max_age_days;
        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                run_session_age_cleanup(&app_state_clone).await;
            }
        });
        tracing::info!(
            "Started session age cleanup task (every hour, max age {} days)",
            max_age_days
        );
    }

    // Run the server with graceful shutdown
    let addr = format!("{}:{}", host, port);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Listening on {}", listener.local_addr()?);

    // Create graceful shutdown handler
    let shutdown_state = app_state.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_state))
        .await?;

    Ok(())
}

/// Handle shutdown signals (SIGTERM, SIGINT) gracefully
/// Broadcasts ServerShutdown message to all clients before returning
async fn shutdown_signal(app_state: Arc<AppState>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received Ctrl+C, initiating graceful shutdown...");
        },
        _ = terminate => {
            tracing::info!("Received SIGTERM, initiating graceful shutdown...");
        },
    }

    // Broadcast shutdown message to all connected clients
    tracing::info!("Broadcasting shutdown notification to all clients...");
    app_state
        .session_manager
        .broadcast_shutdown("Server is restarting".to_string(), 1000);

    // Give clients a moment to receive the message
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    tracing::info!("Shutdown complete");
}

/// Query user spend from DB and broadcast to all connected web clients
async fn broadcast_user_spend_updates(app_state: &Arc<AppState>) {
    use diesel::prelude::*;
    use schema::sessions::dsl::*;
    use shared::{ServerToClient, SessionCost};

    let user_ids = app_state.session_manager.get_all_user_ids();

    for user_id_val in user_ids {
        let Ok(mut conn) = app_state.db_pool.get() else {
            continue;
        };

        // Query per-session costs and token usage for this user (active sessions only)
        type CostRow = (uuid::Uuid, f64, i64, i64, i64, i64);
        let result: Result<Vec<CostRow>, _> = sessions
            .filter(user_id.eq(user_id_val))
            .select((
                id,
                total_cost_usd,
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
            ))
            .load(&mut conn);

        // Get total spend including deleted sessions (matches admin dashboard)
        let total_spend = db::get_user_usage(&mut conn, user_id_val)
            .map(|u| u.cost_usd)
            .unwrap_or(0.0);

        if let Ok(session_costs_data) = result {
            let session_costs_vec: Vec<SessionCost> = session_costs_data
                .into_iter()
                .filter(|&(_, cost, ..)| cost > 0.0) // Only include sessions with costs
                .map(
                    |(sid, cost, inp, outp, cache_create, cache_read)| SessionCost {
                        session_id: sid,
                        total_cost_usd: cost,
                        input_tokens: inp,
                        output_tokens: outp,
                        cache_creation_tokens: cache_create,
                        cache_read_tokens: cache_read,
                    },
                )
                .collect();

            // Only broadcast if there's any spend to report
            if total_spend > 0.0 || !session_costs_vec.is_empty() {
                app_state.session_manager.broadcast_to_user(
                    &user_id_val,
                    ServerToClient::UserSpendUpdate {
                        total_spend_usd: total_spend,
                        session_costs: session_costs_vec,
                    },
                );
            }
        }
    }
}

/// Run retention cleanup: delete old messages and truncate per-session counts
async fn run_retention_cleanup(app_state: &Arc<AppState>) {
    use handlers::retention::{run_retention_cleanup, RetentionConfig};

    let session_ids = app_state.session_manager.drain_pending_truncations();

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for retention cleanup");
        return;
    };

    let config = RetentionConfig::new(
        app_state.message_retention_count,
        app_state.message_retention_days,
    );

    let (age_deleted, count_deleted) = run_retention_cleanup(&mut conn, session_ids, config);

    if age_deleted > 0 || count_deleted > 0 {
        tracing::info!(
            "Retention cleanup complete: {} old, {} over-limit",
            age_deleted,
            count_deleted
        );
    }
}

/// Delete sessions whose last_activity is older than SESSION_MAX_AGE_DAYS
async fn run_session_age_cleanup(app_state: &Arc<AppState>) {
    use diesel::prelude::*;
    use handlers::helpers::delete_session_with_data;

    let max_days = app_state.session_max_age_days;
    if max_days == 0 {
        return;
    }

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for session age cleanup");
        return;
    };

    let cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::days(i64::from(max_days));

    let old_sessions: Vec<models::Session> = match schema::sessions::table
        .filter(schema::sessions::last_activity.lt(cutoff))
        .load(&mut conn)
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to query old sessions: {}", e);
            return;
        }
    };

    if old_sessions.is_empty() {
        return;
    }

    let mut deleted = 0;
    for session in &old_sessions {
        match delete_session_with_data(&mut conn, session, true) {
            Ok(_) => deleted += 1,
            Err(e) => tracing::error!("Failed to delete old session {}: {:?}", session.id, e),
        }
    }

    tracing::info!(
        "Session age cleanup: deleted {} sessions older than {} days",
        deleted,
        max_days
    );

    // Also clean up tokens expired more than 7 days ago
    let token_cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::days(7);
    match diesel::delete(
        schema::proxy_auth_tokens::table
            .filter(schema::proxy_auth_tokens::expires_at.lt(token_cutoff)),
    )
    .execute(&mut conn)
    {
        Ok(0) => {}
        Ok(count) => {
            tracing::info!("Expired token cleanup: deleted {} tokens", count);
        }
        Err(e) => {
            tracing::error!("Failed to delete expired tokens: {}", e);
        }
    }
}

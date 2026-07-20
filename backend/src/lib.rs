// Ratchet for the workspace unwrap/expect deny (#1165 item 8): this crate
// still has production unwrap/expect; remove this allow as it is cleaned.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Agent Portal backend.
//!
//! This is a lib + thin bin: the binary (`main.rs`) just calls [`run`]. Exposing
//! the crate as a library lets integration tests (`backend/tests/`) construct an
//! [`AppState`] and drive [`routes::build_router`] against a real server +
//! Postgres (#1209 item 1, the E2E WS/protocol harness).

pub mod archive;
pub mod auth;
pub mod background;
pub mod config;
pub mod db;
pub mod errors;
pub mod handlers;
pub mod jwt;
pub mod markers;
pub mod models;
pub mod push;
pub mod routes;
pub mod schema;

#[cfg(test)]
pub mod test_support;

use crate::db::DbPool;
use crate::handlers::device_flow::DeviceFlowStore;
use clap::Parser;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower_cookies::Key;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use handlers::websocket::SessionManager;

pub use crate::config::GoogleOAuthClient;

#[derive(Parser, Debug, Clone)]
#[command(name = "agent-portal-backend")]
#[command(about = "Agent Portal backend server")]
#[command(
    after_help = "Source & issues: https://github.com/meawoppl/agent-portal\n  \
                  Report bugs / file issues: https://github.com/meawoppl/agent-portal/issues"
)]
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
    pub oauth_basic_client: Option<GoogleOAuthClient>,
    pub device_flow_store: Option<DeviceFlowStore>,
    pub public_url: String,
    pub cookie_key: Key,
    pub jwt_secret: String,
    pub app_title: String,
    pub splash_text: Option<String>,
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
    /// In-memory image store for serving images via HTTP instead of WebSocket
    pub image_store: handlers::images::ImageStore,
    /// Authority under which per-forward subdomains are served
    /// (docs/PORT_FORWARDING.md). `None` = forwarding disabled.
    pub forward_domain: Option<String>,
    /// Long-term session archive runtime (#1258). `None` = disabled.
    pub archive: Option<Arc<archive::ArchiveRuntime>>,
    /// Non-blocking handle for emitting push notification events
    /// (mobile-apps plan §8). Drained by the dispatcher task spawned in
    /// [`run`]; hooks call `notifications.emit(..)`.
    pub notifications: push::NotificationSender,
    /// VAPID application-server public key served to Web Push clients
    /// (`GET /api/push/vapid-key`). `None` = push unconfigured (endpoint 404s).
    pub vapid_public_key: Option<String>,
    /// Native mobile app-link association payload configuration.
    pub mobile_app_links: config::MobileAppLinksConfig,
}

impl AppState {
    /// Check out a database connection from the pool, mapping pool errors to
    /// [`AppError::DbPool`](crate::errors::AppError::DbPool).
    pub fn conn(&self) -> Result<db::DbConnection, crate::errors::AppError> {
        Ok(self.db_pool.get()?)
    }
}

/// Run the backend server: parse args, build state, serve with graceful
/// shutdown. The binary entry point is a thin wrapper around this.
pub async fn run() -> anyhow::Result<()> {
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

    // Create database pool and run pending migrations automatically
    let pool = db::create_pool()?;
    db::run_migrations_logged(&pool)?;

    // Create device flow store
    let device_flow_store = handlers::device_flow::DeviceFlowStore::default();

    // Create OAuth client (skip in dev mode)
    let oauth_basic_client = config::build_google_oauth_client(args.dev_mode)?;

    // Create test user in dev mode
    if args.dev_mode {
        db::seed_dev_user(&pool)?;
    }

    // Create session manager for WebSocket connections
    let session_manager = SessionManager::new();

    // Push-notification channel (mobile-apps plan §8): the sender lives on
    // AppState and is threaded into the event hooks; the receiver is drained by
    // the dispatcher task spawned once AppState exists (below).
    let (notifications, notification_rx) = push::channel();

    // Mark sessions whose proxies do not reconnect within the grace period.
    // A stale ACTIVE session losing its proxy is an unexpected disconnect, so
    // the sweep emits SessionDisconnected pushes for the sessions it downs.
    background::spawn_stale_session_cleanup(
        pool.clone(),
        session_manager.clone(),
        notifications.clone(),
    );

    // Parse remaining configuration from environment variables
    let config = config::ServerConfig::from_env(args.dev_mode)?;
    let push_transport =
        push::ConfiguredTransport::from_config(config.vapid_private_key, config.native_push)?;

    // Create app state
    let app_state = Arc::new(AppState {
        dev_mode: args.dev_mode,
        db_pool: pool,
        session_manager,
        oauth_basic_client,
        device_flow_store: Some(device_flow_store),
        public_url: config.public_url,
        cookie_key: config.cookie_key,
        jwt_secret: config.jwt_secret,
        app_title: config.app_title,
        splash_text: config.splash_text,
        allowed_email_domain: config.allowed_email_domain,
        allowed_emails: config.allowed_emails,
        message_retention_count: config.message_retention_count,
        message_retention_days: config.message_retention_days,
        session_max_age_days: config.session_max_age_days,
        max_image_mb: config.max_image_mb,
        image_store: handlers::images::ImageStore::new(
            config.image_store_max_bytes,
            config.image_store_ttl,
        ),
        forward_domain: config.forward_domain,
        archive: match config.archive {
            Some(cfg) => Some(Arc::new(
                archive::ArchiveRuntime::new(cfg).map_err(|e| anyhow::anyhow!(e))?,
            )),
            None => None,
        },
        notifications,
        vapid_public_key: config.vapid_public_key,
        mobile_app_links: config.mobile_app_links,
    });

    // Drain notification events and dispatch pushes (mobile-apps plan §8.2).
    push::spawn_dispatcher(app_state.clone(), notification_rx, push_transport);

    // Build our application with routes
    let app = routes::build_router(app_state.clone());

    // Spawn background maintenance tasks
    background::spawn_periodic(
        "user spend broadcast task (every 30 seconds)",
        Duration::from_secs(30),
        app_state.clone(),
        background::broadcast_user_spend_updates,
    );
    background::spawn_periodic(
        "device flow cleanup task (every 60 seconds)",
        Duration::from_secs(60),
        app_state.clone(),
        background::purge_expired_device_codes,
    );
    background::spawn_periodic(
        "message retention task (every 60 seconds)",
        Duration::from_secs(60),
        app_state.clone(),
        background::run_retention_cleanup,
    );
    if app_state.session_max_age_days > 0 {
        background::spawn_periodic(
            &format!(
                "session age cleanup task (every hour, max age {} days)",
                app_state.session_max_age_days
            ),
            Duration::from_secs(3600),
            app_state.clone(),
            background::run_session_age_cleanup,
        );
    }
    background::spawn_periodic(
        "expired proxy token cleanup task (every hour)",
        Duration::from_secs(3600),
        app_state.clone(),
        background::run_expired_token_cleanup,
    );
    background::spawn_periodic(
        "connection liveness sweep (every 30 seconds)",
        Duration::from_secs(handlers::websocket::LIVENESS_SWEEP_INTERVAL_SECS),
        app_state.clone(),
        background::run_liveness_sweep,
    );
    if app_state.archive.is_some() {
        background::spawn_periodic(
            "session archive sweep (every 5 minutes)",
            Duration::from_secs(archive::ARCHIVE_SWEEP_INTERVAL_SECS),
            app_state.clone(),
            background::run_archive_sweep,
        );
    }

    // Run the server with graceful shutdown
    let addr = format!("{}:{}", config.host, config.port);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Listening on {}", listener.local_addr()?);

    // Create graceful shutdown handler
    let shutdown_state = app_state.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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

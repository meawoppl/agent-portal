use axum::{
    routing::{get, post},
    Json, Router,
};
use governor::clock::QuantaInstant;
use governor::middleware::NoOpMiddleware;
use shared::{api::HealthResponse, WsEndpoint};
use std::sync::Arc;
use tower_cookies::CookieManagerLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_governor::GovernorLayer;
use tower_http::cors::{Any, CorsLayer};

use crate::handlers;
use crate::AppState;

pub const ROOT: &str = "/";
pub const DASHBOARD: &str = "/dashboard";
pub const BANNED: &str = "/banned";
pub const ACCESS_DENIED: &str = "/access-denied";

pub const AUTH_GOOGLE: &str = "/api/auth/google";
pub const AUTH_GOOGLE_CALLBACK: &str = "/api/auth/google/callback";
pub const AUTH_ME: &str = "/api/auth/me";
pub const AUTH_REFRESH: &str = "/api/auth/refresh";
pub const AUTH_TOKEN_LOGIN: &str = "/api/auth/token-login";
pub const AUTH_LOGOUT: &str = "/api/auth/logout";
pub const AUTH_DEV_LOGIN: &str = "/api/auth/dev-login";
pub const AUTH_DEVICE_LOGIN: &str = "/api/auth/device-login";

pub const AUTH_DEVICE: &str = "/api/auth/device";
pub const AUTH_DEVICE_CODE: &str = "/api/auth/device/code";
pub const AUTH_DEVICE_POLL: &str = "/api/auth/device/poll";
pub const AUTH_DEVICE_APPROVE: &str = "/api/auth/device/approve";
pub const AUTH_DEVICE_DENY: &str = "/api/auth/device/deny";

/// Per-IP rate limiting layer (via SmartIpKeyExtractor for proxy support)
fn rate_limit(
    per_second: u64,
    burst_size: u32,
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware<QuantaInstant>, axum::body::Body> {
    GovernorLayer::new(Arc::new(
        GovernorConfigBuilder::default()
            .per_second(per_second)
            .burst_size(burst_size)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .unwrap(),
    ))
}

/// Build the full application router: API routes, WebSocket endpoints,
/// rate-limited route groups, embedded frontend assets, and middleware.
pub fn build_router(app_state: Arc<AppState>) -> Router {
    // Setup CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // The forward-host gate is the outermost layer; the main `app_state`
    // binding is consumed by `.with_state` below.
    let app_state_for_forward_gate = app_state.clone();

    // Rate-limited device-code creation. Keep this strict: creating a code is
    // user-visible work and should not be spammed.
    let auth_device_code_routes = Router::new()
        .route(AUTH_DEVICE_CODE, post(handlers::device_flow::device_code))
        .layer(rate_limit(6, 10))
        .with_state(app_state.clone());

    // Rate-limited device polling. The CLI polls every 5s, and this limiter is
    // per-IP, so it must sustain normal polling plus a few machines behind one
    // NAT without draining the burst during a slow browser approval (#1047).
    let auth_device_poll_routes = Router::new()
        .route(AUTH_DEVICE_POLL, post(handlers::device_flow::device_poll))
        .layer(rate_limit(2, 30))
        .with_state(app_state.clone());

    // Rate-limited browser auth routes. These endpoints either initiate OAuth
    // or consume OAuth callback codes, so keep them separate from ordinary
    // authenticated API traffic.
    let auth_login_routes = Router::new()
        .route(AUTH_GOOGLE, get(handlers::auth::login))
        .route(AUTH_GOOGLE_CALLBACK, get(handlers::auth::callback))
        .route(AUTH_DEV_LOGIN, get(handlers::auth::dev_login))
        .route(AUTH_DEVICE_LOGIN, get(handlers::auth::device_login))
        .layer(rate_limit(2, 20))
        .with_state(app_state.clone());

    // Rate-limited device approval actions. The verify page remains unthrottled
    // enough for normal browser refreshes, while mutating approve/deny submits
    // get explicit abuse protection.
    let auth_device_action_routes = Router::new()
        .route(
            AUTH_DEVICE_APPROVE,
            post(handlers::device_flow::device_approve),
        )
        .route(AUTH_DEVICE_DENY, post(handlers::device_flow::device_deny))
        .layer(rate_limit(2, 20))
        .with_state(app_state.clone());

    // Rate-limited download routes
    let download_routes = Router::new()
        .route(
            "/api/download/install.sh",
            get(handlers::downloads::install_script),
        )
        .route(
            "/api/download/proxy",
            get(handlers::downloads::proxy_binary).head(handlers::downloads::proxy_binary),
        )
        .layer(rate_limit(6, 10))
        .with_state(app_state.clone());

    Router::new()
        // Health check endpoint
        .route(
            "/api/health",
            get(|| async {
                Json(HealthResponse {
                    status: "OK".to_string(),
                    version: shared::VERSION.to_string(),
                })
            }),
        )
        // App configuration (public, no auth required)
        .route("/api/config", get(handlers::config::get_config))
        // Session API routes
        .route("/api/sessions", get(handlers::sessions::list_sessions))
        .route(
            "/api/sessions/{id}",
            get(handlers::sessions::get_session).delete(handlers::sessions::delete_session),
        )
        .route(
            "/api/sessions/{id}/stop",
            post(handlers::sessions::stop_session),
        )
        .route(
            "/api/sessions/{id}/pause",
            post(handlers::sessions::pause_session),
        )
        .route(
            "/api/sessions/{id}/resume",
            post(handlers::sessions::resume_session),
        )
        // Session member management routes
        .route(
            "/api/sessions/{id}/members",
            get(handlers::sessions::list_session_members)
                .post(handlers::sessions::add_session_member),
        )
        .route(
            "/api/sessions/{id}/members/{user_id}",
            axum::routing::delete(handlers::sessions::remove_session_member)
                .patch(handlers::sessions::update_session_member_role),
        )
        .route(
            "/api/sessions/{id}/messages",
            get(handlers::messages::list_messages).post(handlers::messages::create_message),
        )
        .route(
            "/api/sessions/{id}/turn-metrics",
            get(handlers::turn_metrics::list_turn_metrics),
        )
        // Port forwarding (docs/PORT_FORWARDING.md): one forward per session,
        // so the mutation/open routes carry no port. Same handlers mounted for
        // browser (cookie) and CLI (Bearer token) path conventions.
        .route(
            "/api/sessions/{id}/forwards",
            get(handlers::forwards::list_forwards)
                .post(handlers::forwards::create_forward)
                .delete(handlers::forwards::delete_forward),
        )
        .route(
            "/api/sessions/{id}/forwards/open",
            get(handlers::forward_proxy::open_forward),
        )
        .route(
            "/api/sessions/{id}/forwards/public",
            axum::routing::patch(handlers::forwards::set_forward_public),
        )
        // The caller's forwards across sessions, for Settings ▸ Forwarding.
        .route("/api/forwards", get(handlers::forwards::list_user_forwards))
        .route(
            "/api/agent/sessions/{id}/forwards",
            get(handlers::forwards::list_forwards)
                .post(handlers::forwards::create_forward)
                .delete(handlers::forwards::delete_forward),
        )
        // Inter-agent messaging: list your sessions, post a message into one
        // (cookie or Bearer-token auth; same-user only).
        .route(
            "/api/agent/sessions",
            get(handlers::agent_comms::list_agent_sessions),
        )
        .route(
            "/api/agent/sessions/{id}/message",
            post(handlers::agent_comms::send_agent_message),
        )
        .route(
            "/api/metrics/recent",
            get(handlers::turn_metrics::list_recent_user_turn_metrics),
        )
        .route(
            "/api/metrics/turns",
            get(handlers::turn_metrics::list_aggregated_turn_metrics),
        )
        // Proxy token management endpoints
        .route(
            "/api/proxy/resolve-session",
            post(handlers::sessions::resolve_proxy_session),
        )
        .route(
            "/api/proxy-tokens",
            get(handlers::proxy_tokens::list_tokens_handler)
                .post(handlers::proxy_tokens::create_token_handler),
        )
        .route(
            "/api/proxy-tokens/{id}",
            axum::routing::delete(handlers::proxy_tokens::revoke_token_handler),
        )
        .route(
            "/api/proxy-tokens/{id}/renew",
            post(handlers::proxy_tokens::renew_token_handler),
        )
        // Image serving endpoint — authenticated (closes #786). Auth check
        // lives in the handler via `extract_user_id`, matching the pattern
        // every other cookie-gated handler in this router uses.
        .route("/api/images/{id}", get(handlers::images::serve_image))
        .route(
            "/api/sessions/{id}/files/pull",
            get(handlers::files::pull_session_file),
        )
        // Scheduled task management endpoints
        .route(
            "/api/scheduled-tasks",
            get(handlers::scheduled_tasks::list_tasks_handler)
                .post(handlers::scheduled_tasks::create_task_handler),
        )
        .route(
            "/api/scheduled-tasks/{id}",
            axum::routing::patch(handlers::scheduled_tasks::update_task_handler)
                .delete(handlers::scheduled_tasks::delete_task_handler),
        )
        .route(
            "/api/scheduled-tasks/{id}/runs",
            get(handlers::scheduled_tasks::list_runs_handler),
        )
        // Push notification subscriptions (mobile-apps plan C1)
        .route(
            "/api/push/subscriptions",
            get(handlers::push::list_subscriptions).post(handlers::push::register_subscription),
        )
        .route(
            "/api/push/subscriptions/{id}",
            axum::routing::delete(handlers::push::delete_subscription),
        )
        .route(
            "/api/push/prefs",
            get(handlers::push::get_prefs).put(handlers::push::put_prefs),
        )
        // Sound settings
        .route(
            "/api/settings/sound",
            get(handlers::sound_settings::get_sound_settings)
                .put(handlers::sound_settings::save_sound_settings),
        )
        .route(AUTH_ME, get(handlers::auth::me))
        .route(AUTH_REFRESH, post(handlers::auth::refresh_token))
        .route(AUTH_TOKEN_LOGIN, post(handlers::auth::token_login))
        .route(AUTH_LOGOUT, get(handlers::auth::logout))
        // Non-rate-limited device flow verify page (form + browser refreshes)
        .route(AUTH_DEVICE, get(handlers::device_flow::device_verify_page))
        // WebSocket routes (paths from ws-bridge endpoint definitions)
        .route(
            shared::SessionEndpoint::PATH,
            get(handlers::websocket::handle_session_websocket),
        )
        .route(
            shared::ImageUploadEndpoint::PATH,
            get(handlers::websocket::handle_image_upload_websocket),
        )
        .route(
            shared::ClientEndpoint::PATH,
            get(handlers::websocket::handle_web_client_websocket),
        )
        .route(
            shared::LauncherEndpoint::PATH,
            get(handlers::websocket::handle_launcher_websocket),
        )
        // Launcher API routes
        .route("/api/launchers", get(handlers::launchers::list_launchers))
        .route(
            "/api/launchers/{launcher_id}/directories",
            get(handlers::launchers::list_directories),
        )
        .route("/api/launch", post(handlers::launchers::launch_session))
        .route(
            "/api/launchers/{launcher_id}/update",
            post(handlers::launchers::update_launcher),
        )
        .route(
            "/api/launchers/{launcher_id}/probe-agents",
            get(handlers::launchers::probe_agents),
        )
        // Admin dashboard routes (admin-only)
        .route("/api/admin/stats", get(handlers::admin::get_stats))
        .route("/api/admin/users", get(handlers::admin::list_users))
        .route(
            "/api/admin/users/{id}",
            axum::routing::patch(handlers::admin::update_user),
        )
        .route("/api/admin/sessions", get(handlers::admin::list_sessions))
        .route(
            "/api/admin/sessions/{id}",
            axum::routing::delete(handlers::admin::delete_session),
        )
        // Admin custom subdomains (docs/PORT_FORWARDING.md)
        .route(
            "/api/admin/subdomains",
            get(handlers::admin_subdomains::list_custom_subdomains)
                .post(handlers::admin_subdomains::create_custom_subdomain),
        )
        .route(
            "/api/admin/subdomains/{label}",
            axum::routing::delete(handlers::admin_subdomains::delete_custom_subdomain),
        )
        .route(
            "/api/admin/forwards",
            get(handlers::admin_subdomains::list_admin_forwards),
        )
        // Add single unified state
        .with_state(app_state)
        // Merge rate-limited route groups
        .merge(auth_device_code_routes)
        .merge(auth_device_poll_routes)
        .merge(auth_login_routes)
        .merge(auth_device_action_routes)
        .merge(download_routes)
        // Serve embedded frontend assets with SPA fallback
        .merge(
            memory_serve::load!()
                .index_file(Some("/index.html"))
                .fallback(Some("/index.html"))
                .fallback_status(axum::http::StatusCode::OK)
                .html_cache_control(memory_serve::CacheControl::NoCache)
                .cache_control(memory_serve::CacheControl::Long)
                .into_router(),
        )
        // Add CORS and cookie management
        .layer(CookieManagerLayer::new())
        .layer(cors)
        // Outermost: requests for `{port}--{session}.{forward domain}` hosts
        // are reverse-proxied to the session's machine and never reach the
        // routes above (docs/PORT_FORWARDING.md).
        .layer(axum::middleware::from_fn_with_state(
            app_state_for_forward_gate,
            handlers::forward_proxy::forward_host_gate,
        ))
}

//! Bootstrap data for the dashboard shell.

use crate::utils::{self, On401};
use shared::api::MeResponse;
use shared::AppConfig;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Stable data fetched once when the dashboard shell mounts.
pub struct DashboardBootstrap {
    pub is_admin: bool,
    pub current_user_id: Option<Uuid>,
    pub app_title: String,
    pub server_version: String,
    /// Short git hash + Pacific build time of the running server (#1386);
    /// empty when an older backend omits them.
    pub git_hash: String,
    pub build_time: String,
    /// Whether the long-term session archive is enabled — drives the
    /// close-session copy (history preserved vs. permanently removed).
    pub archive_enabled: bool,
}

/// Fetch current-user and app configuration data for the dashboard shell.
#[hook]
pub fn use_dashboard_bootstrap() -> DashboardBootstrap {
    let is_admin = use_state(|| false);
    let current_user_id = use_state(|| None::<Uuid>);
    let app_title = use_state(|| "Agent Portal".to_string());
    let server_version = use_state(String::new);
    let git_hash = use_state(String::new);
    let build_time = use_state(String::new);
    let archive_enabled = use_state(|| false);

    {
        let is_admin = is_admin.clone();
        let current_user_id = current_user_id.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(me) = utils::fetch_json::<MeResponse>("/api/auth/me", On401::Ignore).await
                {
                    is_admin.set(me.is_admin);
                    current_user_id.set(Some(me.id));
                }
            });
            || ()
        });
    }

    {
        let app_title = app_title.clone();
        let server_version = server_version.clone();
        let git_hash = git_hash.clone();
        let build_time = build_time.clone();
        let archive_enabled = archive_enabled.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(config) =
                    utils::fetch_json::<AppConfig>("/api/config", On401::Ignore).await
                {
                    app_title.set(config.app_title);
                    server_version.set(config.server_version);
                    git_hash.set(config.git_hash);
                    build_time.set(config.build_time);
                    archive_enabled.set(config.archive_enabled);
                }
            });
            || ()
        });
    }

    DashboardBootstrap {
        is_admin: *is_admin,
        current_user_id: *current_user_id,
        app_title: (*app_title).clone(),
        server_version: (*server_version).clone(),
        git_hash: (*git_hash).clone(),
        build_time: (*build_time).clone(),
        archive_enabled: *archive_enabled,
    }
}

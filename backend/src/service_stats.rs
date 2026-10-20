//! The services monitor: resource usage of the backend process and of each
//! user's connected launchers, pushed to dashboards as
//! `ServerToClient::ServiceStatsUpdate`.
//!
//! The backend samples itself here (one process-wide `SystemMonitor`);
//! launchers sample themselves and report on their heartbeat and, against a
//! backend that advertises `SERVER_CAPABILITY_SYSTEM_STATS`, every few
//! seconds in between. Every user sees the backend row plus their own
//! launchers, nothing of anyone else's.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use shared::{ServerToClient, ServiceKind, ServiceStats};
use uuid::Uuid;

use crate::AppState;

/// How often the backend re-samples itself and refreshes every dashboard.
pub const BROADCAST_PERIOD: Duration = Duration::from_secs(5);

fn monitor() -> &'static Mutex<portal_sysmon::SystemMonitor> {
    static MONITOR: OnceLock<Mutex<portal_sysmon::SystemMonitor>> = OnceLock::new();
    MONITOR.get_or_init(|| Mutex::new(portal_sysmon::SystemMonitor::new()))
}

/// The backend's own row, freshly sampled.
pub fn backend_row(app_state: &AppState) -> ServiceStats {
    let mut guard = monitor().lock().unwrap_or_else(|e| e.into_inner());
    ServiceStats {
        id: "backend".into(),
        kind: ServiceKind::Backend,
        name: "backend".into(),
        hostname: guard.hostname().to_string(),
        sessions: app_state.session_manager.connected_proxy_count() as u32,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        sample: Some(guard.sample()),
    }
}

/// The complete table one user should see right now.
pub fn table_for_user(app_state: &AppState, user_id: Uuid) -> Vec<ServiceStats> {
    app_state
        .session_manager
        .service_stats_for_user(user_id, backend_row(app_state))
}

/// Push the current table to one user's dashboards (after a launcher
/// reported, so the row moves as soon as the reading lands).
pub fn push_to_user(app_state: &AppState, user_id: Uuid) {
    if !app_state.session_manager.has_user_client(user_id) {
        return;
    }
    let services = table_for_user(app_state, user_id);
    app_state
        .session_manager
        .broadcast_to_user(&user_id, ServerToClient::ServiceStatsUpdate { services });
}

/// Periodic task: refresh the backend's own reading and fan the table out to
/// every connected user.
pub async fn broadcast_service_stats(app_state: std::sync::Arc<AppState>) {
    if !app_state.session_manager.has_any_user_clients() {
        // Keep the CPU delta window alive so the first reading after a
        // client connects is meaningful rather than a since-boot average.
        let _ = backend_row(&app_state);
        return;
    }
    let backend = backend_row(&app_state);
    for user_id in app_state.session_manager.get_all_user_ids() {
        let services = app_state
            .session_manager
            .service_stats_for_user(user_id, backend.clone());
        app_state
            .session_manager
            .broadcast_to_user(&user_id, ServerToClient::ServiceStatsUpdate { services });
    }
}

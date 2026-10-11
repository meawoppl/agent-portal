//! Hook for managing session list with automatic polling.

use crate::pages::dashboard::PortalSession;
use crate::portal_client::use_portal;
use crate::utils::On401;
use shared::api::SessionsResponse;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Return value from the use_sessions hook.
pub struct UseSessions {
    /// Current list of sessions
    pub sessions: Vec<PortalSession>,
    /// Whether sessions are currently being loaded (initial load only)
    pub loading: bool,
    /// Manually trigger a refresh
    pub refresh: Callback<()>,
    /// Update the session list directly (for local modifications)
    pub set_sessions: Callback<Vec<PortalSession>>,
}

/// Hook for fetching and polling session list.
///
/// Automatically fetches sessions from the portal in context on mount and
/// polls every 5 seconds. Handles 401 responses by redirecting to logout.
///
/// # Returns
/// * `UseSessions` - The current sessions, loading state, and control callbacks
///
#[hook]
pub fn use_sessions() -> UseSessions {
    let portal = use_portal();
    let sessions = use_state(Vec::<PortalSession>::new);
    let loading = use_state(|| true);
    let refresh_trigger = use_state(|| 0u32);

    // Fetch sessions callback
    let fetch_sessions = {
        let sessions = sessions.clone();
        let loading = loading.clone();

        Callback::from(move |set_loading: bool| {
            let sessions = sessions.clone();
            let loading = loading.clone();
            let portal = portal.clone();

            spawn_local(async move {
                // 401 means the session is invalid; the helper redirects to logout.
                match portal
                    .fetch_json::<SessionsResponse>("/api/sessions", On401::Logout)
                    .await
                {
                    Ok(data) => {
                        sessions.set(
                            data.sessions
                                .into_iter()
                                .map(|info| PortalSession::new(portal.clone(), info))
                                .collect(),
                        );
                    }
                    Err(e) => {
                        log::error!("Failed to fetch sessions: {}", e);
                    }
                }
                if set_loading {
                    loading.set(false);
                }
            });
        })
    };

    // Initial fetch
    {
        let fetch_sessions = fetch_sessions.clone();
        use_effect_with((), move |_| {
            fetch_sessions.emit(true);
            || ()
        });
    }

    // Polling every 5 seconds
    {
        let fetch_sessions = fetch_sessions.clone();
        use_effect_with((), move |_| {
            let interval = gloo::timers::callback::Interval::new(5_000, move || {
                fetch_sessions.emit(false);
            });
            move || drop(interval)
        });
    }

    // Refresh trigger effect
    {
        let fetch_sessions = fetch_sessions.clone();
        let refresh = *refresh_trigger;
        use_effect_with(refresh, move |_| {
            if refresh > 0 {
                fetch_sessions.emit(false);
            }
            || ()
        });
    }

    // Manual refresh callback
    let refresh = {
        let refresh_trigger = refresh_trigger.clone();
        Callback::from(move |_| {
            refresh_trigger.set(*refresh_trigger + 1);
        })
    };

    // Direct setter for local modifications
    let set_sessions = {
        let sessions = sessions.clone();
        Callback::from(move |new_sessions: Vec<PortalSession>| {
            sessions.set(new_sessions);
        })
    };

    UseSessions {
        sessions: (*sessions).clone(),
        loading: *loading,
        refresh,
        set_sessions,
    }
}

pub mod audio;
mod components;
mod health_timer;
mod hooks;
mod pages;
pub mod utils;

/// Curated, presentation-only facade for reusing the portal's message
/// renderers from a second WASM app — specifically the standalone archive
/// viewer (#1288), which links this crate as an rlib to render archived
/// transcripts with the real renderer family instead of reimplementing two
/// agent protocols.
///
/// Recipe for a read-only transcript view:
/// 1. Build a [`viewer_api::RenderedMessage`] per archived line:
///    `RenderedMessage::new(raw_content_json_string, Some(portal_meta))` —
///    `content` is the stored wire JSON exactly as archived; `meta` carries
///    `created_at` for timestamps.
/// 2. `group_messages(&messages, agent_type, current_user_id)` →
///    [`viewer_api::MessageGroup`]s.
/// 3. Render each with [`viewer_api::MessageGroupRenderer`]. All live-session
///    props (`turn_metrics`, `continuation_statuses`,
///    `on_schedule_continuation`) are `#[prop_or_default]` and no-op when
///    omitted, so a viewer passes only `group`, `session_id`, `agent_type`.
///
/// The consuming app must also include the portal's message CSS (see
/// `frontend/styles/` — notably `messages.css` and the renderer styles) for
/// the output to look like the portal.
///
/// Additions to this facade must remain presentation-only: no live-session,
/// WebSocket, or auth types may pass through this surface.
pub mod viewer_api {
    pub use crate::components::message_renderer::{
        group_is_turn_terminator, group_messages, thinking_chip_starts, GroupCategory,
        MessageGroup, MessageGroupRenderer, MessageGroupRendererProps, RenderedMessage,
    };

    /// Compile-level guarantee that the facade is sufficient for an external
    /// read-only viewer: constructs messages, groups them, and builds the
    /// renderer props exactly the way the archive viewer will — using only
    /// items re-exported above (no live-session machinery).
    #[cfg(test)]
    mod facade_sufficiency {
        use super::*;

        #[test]
        fn archived_lines_group_and_props_build_without_live_state() {
            let messages = vec![
                RenderedMessage::new(r#"{"type":"user","content":"hello"}"#.to_string(), None),
                RenderedMessage::new(
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#
                        .to_string(),
                    None,
                ),
            ];

            let groups = group_messages(&messages, shared::AgentType::Claude, None);
            assert!(!groups.is_empty());

            // Props must build with ONLY the presentation-required fields —
            // every live-session prop defaults.
            let _props = ::yew::props!(MessageGroupRendererProps {
                group: groups[0].clone(),
                session_id: uuid::Uuid::nil(),
            });
        }
    }
}

/// Application version — derived at build time from the git commit count
/// (see `shared::VERSION` / `shared/build.rs`, issue #1096).
pub const VERSION: &str = shared::VERSION;

use health_timer::HealthTimerReminder;
use pages::{
    access_denied::AccessDeniedPage, admin::AdminPage, banned::BannedPage,
    dashboard::DashboardPage, demo::DemoPage, settings::SettingsPage, splash::SplashPage,
};
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/dashboard")]
    Dashboard,
    #[at("/demo")]
    Demo,
    #[at("/settings")]
    Settings,
    #[at("/admin")]
    Admin,
    #[at("/banned")]
    Banned,
    #[at("/access-denied")]
    AccessDenied,
}

/// Wrapper for /admin route — provides back-navigation on_close callback
#[function_component(AdminRoute)]
fn admin_route() -> Html {
    let Some(navigator) = use_navigator() else {
        return html! {};
    };
    let on_close = Callback::from(move |_| navigator.push(&Route::Dashboard));
    html! { <AdminPage on_close={on_close} /> }
}

/// Wrapper for /settings route — provides back-navigation on_close callback
#[function_component(SettingsRoute)]
fn settings_route() -> Html {
    let Some(navigator) = use_navigator() else {
        return html! {};
    };
    let on_close = Callback::from(move |_| navigator.push(&Route::Dashboard));
    html! { <SettingsPage on_close={on_close} /> }
}

fn switch(routes: Route) -> Html {
    match routes {
        Route::Home => html! { <SplashPage /> },
        Route::Dashboard => html! { <DashboardPage /> },
        Route::Demo => html! { <DemoPage /> },
        Route::Settings => html! { <SettingsRoute /> },
        Route::Admin => html! { <AdminRoute /> },
        Route::Banned => html! { <BannedPage /> },
        Route::AccessDenied => html! { <AccessDeniedPage /> },
    }
}

#[function_component(App)]
fn app() -> Html {
    html! {
        <BrowserRouter>
            <Switch<Route> render={switch} />
            <HealthTimerReminder />
        </BrowserRouter>
    }
}

/// Register the PWA service worker (`frontend/sw.js`).
///
/// The `?v=` query carries `shared::VERSION` (compiled into the WASM) so the
/// worker derives a per-deploy cache name from `self.location.search` and drops
/// stale caches on activate — the guard against serving stale WASM after a
/// deploy. Fire-and-forget: registration is best-effort and must never block or
/// panic app startup, so we feature-detect and ignore the returned Promise.
fn register_service_worker() {
    use wasm_bindgen::JsValue;

    let Some(window) = web_sys::window() else {
        return;
    };
    let navigator = window.navigator();
    // `serviceWorker` is undefined on unsupported browsers / insecure contexts.
    if !js_sys::Reflect::has(&navigator, &JsValue::from_str("serviceWorker")).unwrap_or(false) {
        return;
    }
    let url = format!("/sw.js?v={}", shared::VERSION);
    let _ = navigator.service_worker().register(&url);
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn run_app() {
    wasm_logger::init(wasm_logger::Config::default());
    register_service_worker();
    yew::Renderer::<App>::new().render();
}

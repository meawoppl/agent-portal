pub mod audio;
mod components;
mod health_timer;
mod hooks;
mod pages;
pub mod utils;

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

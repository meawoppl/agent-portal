mod browser_protocol;
mod components;
mod dashboard_rpc;
mod federation;
mod health_timer;
mod hooks;
mod linked_credentials;
mod linked_media;
mod notification_sound;
mod pages;
mod portal_client;
#[cfg(test)]
pub mod test_fixtures;
mod ui_theme;
pub mod utils;

/// Application version — derived at build time from the git commit count
/// (see `shared::VERSION` / `shared/build.rs`, issue #1096).
pub const VERSION: &str = shared::VERSION;

use health_timer::HealthTimerReminder;
use pages::{
    access_denied::AccessDeniedPage, admin::AdminPage, banned::BannedPage,
    dashboard::DashboardPage, demo::DemoPage, history::HistoryBrowserPage,
    history::HistoryTranscriptPage, settings::SettingsPage, splash::SplashPage,
};
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/dashboard")]
    Dashboard,
    #[at("/aperture")]
    Aperture,
    #[at("/demo")]
    Demo,
    #[at("/settings")]
    Settings,
    #[at("/secrets")]
    Secrets,
    #[at("/secrets/authorize")]
    SecretAuthorize,
    #[at("/secrets/jobs/:id")]
    SecretJob { id: String },
    #[at("/history")]
    History,
    #[at("/history/:user/:session")]
    HistorySession { user: String, session: String },
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
    let job_id = use_location()
        .and_then(|location| {
            location
                .query::<std::collections::HashMap<String, String>>()
                .ok()
        })
        .and_then(|query| query.get("secret_job").cloned());
    let on_close = Callback::from(move |_| navigator.push(&Route::Dashboard));
    html! { <SettingsPage on_close={on_close} secret_job_id={job_id} /> }
}

#[derive(Properties, PartialEq)]
struct LegacySecretsProps {
    #[prop_or_default]
    job_id: Option<String>,
}

/// Keep old bookmarks and cleanup-job links useful while Settings owns the UI.
#[function_component(LegacySecretsRoute)]
fn legacy_secrets_route(props: &LegacySecretsProps) -> Html {
    let navigator = use_navigator();
    use_effect_with((navigator, props.job_id.clone()), |(navigator, job_id)| {
        if let Some(navigator) = navigator {
            let mut query = vec![("tab", "secrets")];
            if let Some(id) = job_id {
                query.push(("secret_job", id.as_str()));
            }
            let _ = navigator.replace_with_query(&Route::Settings, &query);
        }
    });
    html! {}
}

fn switch(routes: Route) -> Html {
    match routes {
        Route::Home => html! { <SplashPage /> },
        Route::Dashboard => html! { <DashboardPage /> },
        Route::Demo => html! { <DemoPage /> },
        Route::Aperture => html! { <pages::aperture::AperturePage /> },
        Route::Settings => html! { <SettingsRoute /> },
        Route::Secrets => html! { <LegacySecretsRoute /> },
        Route::SecretAuthorize => html! { <pages::secrets::SecretAuthorizePage /> },
        Route::SecretJob { id } => html! { <LegacySecretsRoute job_id={Some(id)} /> },
        Route::History => html! { <HistoryBrowserPage /> },
        Route::HistorySession { user, session } => html! {
            <HistoryTranscriptPage {user} {session} />
        },
        Route::Admin => html! { <AdminRoute /> },
        Route::Banned => html! { <BannedPage /> },
        Route::AccessDenied => html! { <AccessDeniedPage /> },
    }
}

#[function_component(App)]
fn app() -> Html {
    html! {
        <federation::FederationProvider>
            <hooks::ClientWebSocketProvider>
            <BrowserRouter>
                <Switch<Route> render={switch} />
                <HealthTimerReminder />
            </BrowserRouter>
            </hooks::ClientWebSocketProvider>
        </federation::FederationProvider>
    }
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn run_app() {
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<App>::new().render();
}

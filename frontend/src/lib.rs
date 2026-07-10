pub mod audio;
mod components;
mod hooks;
mod pages;
pub mod utils;

/// Application version — derived at build time from the git commit count
/// (see `shared::VERSION` / `shared/build.rs`, issue #1096).
pub const VERSION: &str = shared::VERSION;

use pages::{
    access_denied::AccessDeniedPage, admin::AdminPage, agent_messaging::AgentMessagingPage,
    banned::BannedPage, dashboard::DashboardPage, settings::SettingsPage, splash::SplashPage,
};
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/dashboard")]
    Dashboard,
    #[at("/settings")]
    Settings,
    #[at("/admin")]
    Admin,
    #[at("/agents")]
    Agents,
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
        Route::Settings => html! { <SettingsRoute /> },
        Route::Admin => html! { <AdminRoute /> },
        Route::Agents => html! { <AgentMessagingPage /> },
        Route::Banned => html! { <BannedPage /> },
        Route::AccessDenied => html! { <AccessDeniedPage /> },
    }
}

#[function_component(App)]
fn app() -> Html {
    html! {
        <BrowserRouter>
            <Switch<Route> render={switch} />
        </BrowserRouter>
    }
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn run_app() {
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<App>::new().render();
}

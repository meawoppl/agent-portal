//! Standalone archive-viewer WASM app (#1288, PR 3a).
//!
//! Renders archived agent transcripts using the *real* portal renderers via
//! [`frontend::viewer_api`]. Hash-routed so it can be served as static files
//! from any path: `#/` is the session browser, `#/session/{user}/{session}` is
//! the transcript view. The JSON API it fetches is provided by the
//! `portal-archive serve` subcommand (PR 3b), which also embeds this `dist/`.

mod api;
mod browser;
mod filters;
mod media_rewrite;
mod transcript;

use browser::SessionBrowser;
use transcript::TranscriptView;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/session/:user/:session")]
    Session { user: String, session: String },
    #[not_found]
    #[at("/404")]
    NotFound,
}

fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <SessionBrowser /> },
        Route::Session { user, session } => {
            let key = format!("{user}/{session}");
            html! { <TranscriptView {key} {user} {session} /> }
        }
        Route::NotFound => html! {
            <div class="viewer-empty">
                <h2>{ "Page not found" }</h2>
                <a href="#/">{ "Back to sessions" }</a>
            </div>
        },
    }
}

#[function_component(App)]
fn app() -> Html {
    html! {
        <HashRouter>
            <Switch<Route> render={switch} />
        </HashRouter>
    }
}

fn main() {
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<App>::new().render();
}

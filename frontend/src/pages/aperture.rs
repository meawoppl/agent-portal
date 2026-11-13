//! Preserve client-side navigation and observer hashes after the exhibit moves.
use yew::prelude::*;

const EXHIBIT_URL: &str = "https://meawoppl.github.io/agent-portal/";

#[function_component(AperturePage)]
pub fn aperture_page() -> Html {
    use_effect_with((), |_| {
        if let Some(window) = web_sys::window() {
            let hash = window.location().hash().unwrap_or_default();
            let _ = window.location().replace(&format!("{EXHIBIT_URL}{hash}"));
        }
        || ()
    });
    html! { <main><p>{"The Aperture exhibit has moved."}</p><a href={EXHIBIT_URL}>{"Open the exhibit"}</a></main> }
}

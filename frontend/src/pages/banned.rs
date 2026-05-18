use serde::{Deserialize, Serialize};
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
struct BannedQuery {
    #[serde(default)]
    reason: Option<String>,
}

#[function_component(BannedPage)]
pub fn banned_page() -> Html {
    let reason = use_location()
        .and_then(|loc| loc.query::<BannedQuery>().ok())
        .and_then(|q| q.reason)
        .unwrap_or_else(|| "No reason provided".to_string());

    html! {
        <div class="banned-container">
            <div class="banned-content">
                <div class="banned-icon">{ "🚫" }</div>
                <h1>{ "Account Suspended" }</h1>
                <p class="banned-message">
                    { "Your account has been suspended and you are unable to access this service." }
                </p>
                <div class="banned-reason">
                    <h3>{ "Reason:" }</h3>
                    <p>{ reason }</p>
                </div>
                <p class="banned-contact">
                    { "If you believe this is an error, please contact " }
                    <a href="mailto:support@txcl.io">{ "support@txcl.io" }</a>
                </p>
            </div>
        </div>
    }
}

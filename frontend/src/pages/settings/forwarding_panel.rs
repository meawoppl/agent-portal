//! Settings ▸ Forwarding: the user's active port forwards, each with a
//! public/private toggle (docs/PORT_FORWARDING.md). A public forward serves
//! its subdomain to anyone with the URL, no portal login required.

use gloo_net::http::Request;
use shared::api::{SetForwardPublicRequest, UserForwardInfo, UserForwardsResponse};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::utils::{self, On401};

#[function_component(ForwardingPanel)]
pub fn forwarding_panel() -> Html {
    let forwards = use_state(Vec::<UserForwardInfo>::new);
    let loading = use_state(|| true);

    let reload = {
        let forwards = forwards.clone();
        let loading = loading.clone();
        move || {
            let forwards = forwards.clone();
            let loading = loading.clone();
            spawn_local(async move {
                let data =
                    utils::fetch_json::<UserForwardsResponse>("/api/forwards", On401::Ignore)
                        .await
                        .map(|d| d.forwards)
                        .unwrap_or_default();
                forwards.set(data);
                loading.set(false);
            });
        }
    };

    {
        let reload = reload.clone();
        use_effect_with((), move |_| {
            reload();
            || ()
        });
    }

    let on_toggle = {
        let reload = reload.clone();
        Callback::from(move |(session_id, public): (Uuid, bool)| {
            let reload = reload.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/sessions/{session_id}/forwards/public"));
                if let Ok(req) = Request::patch(&url).json(&SetForwardPublicRequest { public }) {
                    let _ = req.send().await;
                }
                // Refetch so the row reflects the server (and any concurrent
                // registration/revocation).
                reload();
            });
        })
    };

    html! {
        <section class="forwarding-section">
            <div class="section-header">
                <h2>{ "Port Forwarding" }</h2>
                <p class="section-description">
                    { "Local HTTP services your agents have exposed. Toggle a forward \
                       public to let anyone with its URL reach it without signing in." }
                </p>
            </div>

            if *loading {
                <div class="loading">
                    <div class="spinner"></div>
                    <p>{ "Loading forwards…" }</p>
                </div>
            } else if forwards.is_empty() {
                <p class="empty-state">
                    { "No active forwards. An agent creates one with " }
                    <code>{ "agent-portal forward <port>" }</code>{ "." }
                </p>
            } else {
                <div class="forwarding-list">
                    { for forwards.iter().map(|f| {
                        let on_toggle = on_toggle.clone();
                        let session_id = f.session_id;
                        let next_public = !f.public;
                        let onchange = Callback::from(move |_: Event| {
                            on_toggle.emit((session_id, next_public));
                        });
                        html! {
                            <div class="forwarding-row" key={session_id.to_string()}>
                                <div class="forwarding-meta">
                                    <span class="forwarding-name">{ &f.session_name }</span>
                                    <a
                                        class="forwarding-url"
                                        href={f.url.clone()}
                                        target="_blank"
                                        rel="noopener noreferrer"
                                    >{ format!("{}  (:{})", f.url, f.port) }</a>
                                </div>
                                <label class="toggle-label forwarding-public">
                                    <span class={classes!(
                                        "forwarding-badge",
                                        f.public.then_some("is-public"),
                                    )}>
                                        { if f.public { "Public" } else { "Private" } }
                                    </span>
                                    <input
                                        type="checkbox"
                                        checked={f.public}
                                        {onchange}
                                    />
                                </label>
                            </div>
                        }
                    }) }
                </div>
            }
        </section>
    }
}

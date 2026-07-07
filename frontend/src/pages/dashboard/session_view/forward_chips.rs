//! Active port-forward chips for the session header (docs/PORT_FORWARDING.md).
//!
//! Fetches `GET /api/sessions/{id}/forwards` on mount and whenever `refresh`
//! bumps (the parent bumps it on a `ForwardsChanged` WS frame). Each chip
//! opens the forward through the portal `/open` handoff endpoint in a new tab;
//! the session owner also gets a revoke `×`.

use gloo_net::http::Request;
use shared::api::{ForwardInfo, SessionForwardsResponse};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::utils::{self, api_url, fetch_json, On401};

#[derive(Properties, PartialEq)]
pub struct ForwardChipsProps {
    pub session_id: Uuid,
    /// True when the viewer owns the session (owner-only revoke).
    pub is_owner: bool,
    /// Bumped by the parent on a `ForwardsChanged` frame to trigger a refetch.
    pub refresh: u32,
}

#[function_component(ForwardChips)]
pub fn forward_chips(props: &ForwardChipsProps) -> Html {
    let forwards = use_state(Vec::<ForwardInfo>::new);

    // Refetch on mount and whenever (session, refresh) changes.
    {
        let forwards = forwards.clone();
        let session_id = props.session_id;
        use_effect_with((session_id, props.refresh), move |_| {
            spawn_local(async move {
                if let Ok(data) = fetch_json::<SessionForwardsResponse>(
                    &format!("/api/sessions/{session_id}/forwards"),
                    On401::Ignore,
                )
                .await
                {
                    forwards.set(data.forwards);
                }
            });
            || ()
        });
    }

    if forwards.is_empty() {
        return Html::default();
    }

    let revoke = {
        let forwards = forwards.clone();
        let session_id = props.session_id;
        Callback::from(move |port: u16| {
            let forwards = forwards.clone();
            spawn_local(async move {
                let url = api_url(&format!("/api/sessions/{session_id}/forwards/{port}"));
                if Request::delete(&url).send().await.is_ok() {
                    // Optimistic: drop it locally; the ForwardsChanged frame
                    // will reconcile if the server disagrees.
                    forwards.set(
                        forwards
                            .iter()
                            .filter(|f| f.port != port)
                            .cloned()
                            .collect(),
                    );
                }
            });
        })
    };

    html! {
        <span class="session-forwards">
            { for forwards.iter().map(|f| {
                let open_url = utils::api_url(&format!(
                    "/api/sessions/{}/forwards/{}/open",
                    props.session_id, f.port
                ));
                let port = f.port;
                let on_revoke = revoke.clone();
                html! {
                    <span class="forward-chip" key={port} title={f.url.clone()}>
                        <a href={open_url} target="_blank" rel="noopener noreferrer">
                            { format!(":{port} ↗") }
                        </a>
                        if props.is_owner {
                            <button
                                class="forward-chip-revoke"
                                title="Stop forwarding this port"
                                onclick={Callback::from(move |_| on_revoke.emit(port))}
                            >{ "×" }</button>
                        }
                    </span>
                }
            }) }
        </span>
    }
}

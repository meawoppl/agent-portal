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
    // The forwards are tagged with the session they belong to. SessionView
    // reuses this component instance across session switches, so an in-flight
    // or already-loaded fetch for the previous session must never render: we
    // only show `state` when its tagged id matches the current prop. This
    // closes both stale paths — old chips carrying the new session's URLs, and
    // an out-of-order fetch overwriting a newer session's chips — without a
    // clear-then-refetch flicker on the `refresh` bump.
    let state = use_state(|| (Uuid::nil(), Vec::<ForwardInfo>::new()));

    // Refetch on mount and whenever (session, refresh) changes.
    {
        let state = state.clone();
        let session_id = props.session_id;
        use_effect_with((session_id, props.refresh), move |_| {
            spawn_local(async move {
                let forwards = fetch_json::<SessionForwardsResponse>(
                    &format!("/api/sessions/{session_id}/forwards"),
                    On401::Ignore,
                )
                .await
                .map(|data| data.forwards)
                .unwrap_or_default();
                // Tag with the session this fetch was for; a late arrival for
                // an old session is ignored by the render guard below.
                state.set((session_id, forwards));
            });
            || ()
        });
    }

    // Ignore results tagged for a different (stale) session.
    let forwards: &[ForwardInfo] = if state.0 == props.session_id {
        &state.1
    } else {
        &[]
    };
    if forwards.is_empty() {
        return Html::default();
    }

    let revoke = {
        let state = state.clone();
        let session_id = props.session_id;
        Callback::from(move |port: u16| {
            let state = state.clone();
            spawn_local(async move {
                let url = api_url(&format!("/api/sessions/{session_id}/forwards/{port}"));
                if Request::delete(&url).send().await.is_ok() {
                    // Optimistic: drop it locally (re-tagging with this
                    // session); the ForwardsChanged frame reconciles if the
                    // server disagrees.
                    let remaining = state.1.iter().filter(|f| f.port != port).cloned().collect();
                    state.set((session_id, remaining));
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

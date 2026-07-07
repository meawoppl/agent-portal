//! Active port-forward chips for the session header (docs/PORT_FORWARDING.md).
//!
//! Fetches `GET /api/sessions/{id}/forwards` on mount and whenever `refresh`
//! bumps (the parent bumps it on a `ForwardsChanged` WS frame). Each chip
//! opens the forward through the portal `/open` handoff endpoint in a new tab;
//! the session owner also gets a revoke `×`.

use std::cell::Cell;
use std::rc::Rc;

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
    // Forwards are tagged with the session they belong to (belt) and every
    // fetch is guarded by a cancellation flag its effect trips on cleanup
    // (suspenders). SessionView reuses this component instance across session
    // switches, so both are needed: the tag stops a stale result from ever
    // *rendering* under the new session's URLs, and the cancel flag stops a
    // superseded fetch from *writing* at all — otherwise a late fetch for the
    // old session could clobber the new session's already-loaded chips, which
    // might never refetch and would vanish indefinitely.
    let state = use_state(|| (Uuid::nil(), Vec::<ForwardInfo>::new()));

    // Refetch on mount and whenever (session, refresh) changes.
    {
        let state = state.clone();
        let session_id = props.session_id;
        use_effect_with((session_id, props.refresh), move |_| {
            let cancelled = Rc::new(Cell::new(false));
            let guard = cancelled.clone();
            spawn_local(async move {
                let forwards = fetch_json::<SessionForwardsResponse>(
                    &format!("/api/sessions/{session_id}/forwards"),
                    On401::Ignore,
                )
                .await
                .map(|data| data.forwards)
                .unwrap_or_default();
                // Superseded by a newer (session, refresh) — don't write.
                if !guard.get() {
                    state.set((session_id, forwards));
                }
            });
            // Cleanup runs before the next effect (deps changed) and on
            // unmount, tripping the flag for this now-stale fetch.
            move || cancelled.set(true)
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

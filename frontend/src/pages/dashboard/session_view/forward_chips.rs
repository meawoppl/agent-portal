//! Active port-forward chips for the session header (docs/PORT_FORWARDING.md).
//!
//! Fetches `GET /api/sessions/{id}/forwards` on mount and whenever `refresh`
//! bumps (the parent bumps it on a `ForwardsChanged` WS frame). Each chip emits
//! its [`ForwardInfo`] to the session view, which owns the split-surface iframe;
//! the session owner also gets a revoke `×`.

use std::cell::Cell;
use std::rc::Rc;

use gloo_net::http::Request;
use shared::api::{ForwardInfo, SessionForwardsResponse};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::utils::{api_url, fetch_json, On401};

#[derive(Properties, PartialEq)]
pub struct ForwardChipsProps {
    pub session_id: Uuid,
    /// True when the viewer owns the session (owner-only revoke).
    pub is_owner: bool,
    /// Bumped by the parent on a `ForwardsChanged` frame to trigger a refetch.
    pub refresh: u32,
    /// Open the selected forward in the session-owned surface host.
    pub on_open: Callback<ForwardInfo>,
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
        Callback::from(move |_: ()| {
            let state = state.clone();
            spawn_local(async move {
                // A session has one forward; DELETE takes no port.
                let url = api_url(&format!("/api/sessions/{session_id}/forwards"));
                if Request::delete(&url).send().await.is_ok() {
                    // Optimistic: clear locally (keeping this session's tag);
                    // the ForwardsChanged frame reconciles if the server
                    // disagrees.
                    state.set((session_id, Vec::new()));
                }
            });
        })
    };

    html! {
        <span class="session-forwards">
            { for forwards.iter().map(|f| {
                let on_revoke = revoke.clone();
                let on_open = props.on_open.clone();
                let forward = f.clone();
                // Health is conveyed by color alone: pulsing green = last
                // probe saw a listener, flat red = connection refused,
                // neutral = no verdict yet (proxy offline or pre-probe).
                // Updates ride ForwardsChanged.
                let health_class = match f.listening {
                    Some(true) => Some("is-up"),
                    Some(false) => Some("is-down"),
                    None => None,
                };
                // Tooltip names the bound app when the probe resolved it.
                let health_title = match &f.process {
                    Some(process) => format!("{process} — {}", f.url),
                    None => f.url.clone(),
                };
                html! {
                    <span
                        class={classes!("forward-chip", health_class)}
                        key={f.port}
                        title={health_title}
                    >
                        // Click opens the session-owned split surface; "Visit
                        // site" inside the surface goes to the full page.
                        <button
                            type="button"
                            class="forward-chip-open"
                            onclick={Callback::from(move |_| on_open.emit(forward.clone()))}
                        >
                            { format!(":{} ↗", f.port) }
                        </button>
                        if props.is_owner {
                            <button
                                class="forward-chip-revoke"
                                title="Stop forwarding"
                                onclick={Callback::from(move |_| on_revoke.emit(()))}
                            >{ "×" }</button>
                        }
                    </span>
                }
            }) }
        </span>
    }
}

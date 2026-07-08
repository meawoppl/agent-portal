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

    // Preview overlay: open/collapsed are independent so collapsing keeps the
    // iframe mounted (the embedded app keeps running; expand restores it
    // where it was). Closed on session switch below.
    let preview_open = use_state(|| false);
    let preview_collapsed = use_state(|| false);
    {
        let preview_open = preview_open.clone();
        use_effect_with(props.session_id, move |_| {
            preview_open.set(false);
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

    let open_url = utils::api_url(&format!("/api/sessions/{}/forwards/open", props.session_id));

    let toggle_preview = {
        let preview_open = preview_open.clone();
        let preview_collapsed = preview_collapsed.clone();
        Callback::from(move |e: MouseEvent| {
            e.prevent_default();
            if *preview_open {
                preview_open.set(false);
            } else {
                preview_open.set(true);
                preview_collapsed.set(false);
            }
        })
    };
    let toggle_collapse = {
        let preview_collapsed = preview_collapsed.clone();
        Callback::from(move |_: MouseEvent| preview_collapsed.set(!*preview_collapsed))
    };
    let close_preview = {
        let preview_open = preview_open.clone();
        Callback::from(move |_: MouseEvent| preview_open.set(false))
    };

    let forward = &forwards[0];
    html! {
        <>
        <span class="session-forwards">
            { for forwards.iter().map(|f| {
                let on_revoke = revoke.clone();
                html! {
                    <span class="forward-chip" key={f.port} title={f.url.clone()}>
                        // Click opens the inline preview overlay; "Visit site"
                        // inside it goes to the full page.
                        <a href={open_url.clone()} onclick={toggle_preview.clone()}>
                            { format!(":{} ↗", f.port) }
                        </a>
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
        if *preview_open {
            <div class="forward-preview">
                <div class="forward-preview-bar">
                    <button
                        class="forward-preview-btn"
                        title={ if *preview_collapsed { "Expand" } else { "Collapse" } }
                        onclick={toggle_collapse}
                    >
                        { if *preview_collapsed { "▸" } else { "▾" } }
                    </button>
                    <span class="forward-preview-title">
                        { format!(":{} — {}", forward.port, forward.url) }
                    </span>
                    <a
                        class="forward-preview-visit"
                        href={open_url.clone()}
                        target="_blank"
                        rel="noopener noreferrer"
                    >{ "Visit site ↗" }</a>
                    <button
                        class="forward-preview-btn forward-preview-close"
                        title="Close preview"
                        onclick={close_preview}
                    >{ "×" }</button>
                </div>
                // Kept mounted while collapsed so the embedded app keeps its
                // state; only the height changes.
                <iframe
                    class={classes!(
                        "forward-preview-frame",
                        preview_collapsed.then_some("collapsed"),
                    )}
                    src={open_url}
                    title="Forwarded app preview"
                />
            </div>
        }
        </>
    }
}

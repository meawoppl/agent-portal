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
use yew::events::PointerEvent;
use yew::prelude::*;

use crate::utils::{self, api_url, fetch_json, On401};

/// Height reserved for the preview title bar when converting a resize
/// pointer position into an iframe height.
const PREVIEW_BAR_PX: f64 = 34.0;

/// An in-progress pointer interaction with the preview overlay. Drag carries
/// the pointer's offset inside the panel so the panel doesn't jump to put its
/// corner under the cursor.
#[derive(Clone, Copy, PartialEq)]
enum PreviewInteraction {
    Drag { dx: f64, dy: f64 },
    Resize,
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi.max(lo))
}

fn viewport() -> (f64, f64) {
    let win = web_sys::window();
    let dim = |v: Option<Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>>| {
        v.and_then(|r| r.ok()).and_then(|j| j.as_f64())
    };
    (
        dim(win.as_ref().map(|w| w.inner_width())).unwrap_or(1280.0),
        dim(win.map(|w| w.inner_height())).unwrap_or(800.0),
    )
}

/// Capture the pointer on the element that received `e`, so pointermove keeps
/// arriving even when the cursor crosses the embedded iframe (which would
/// otherwise swallow the stream mid-drag).
fn capture_pointer(e: &PointerEvent) {
    let target: web_sys::Element = e.target_unchecked_into();
    let _ = target.set_pointer_capture(e.pointer_id());
}

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
    // Panel geometry (left, top, width, iframe height) in px — dragged by the
    // title bar, resized by the corner grip. Survives collapse/expand.
    let geom = use_state(|| (12.0f64, 52.0f64, 760.0f64, 480.0f64));
    let interaction = use_state(|| None::<PreviewInteraction>);
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

    // Drag (title bar) and resize (corner grip) share the pointer-capture
    // pattern: capture on pointerdown so pointermove keeps flowing over the
    // iframe, apply geometry on move, release state on up/cancel.
    let drag_start = {
        let geom = geom.clone();
        let interaction = interaction.clone();
        Callback::from(move |e: PointerEvent| {
            e.prevent_default();
            capture_pointer(&e);
            let (x, y, ..) = *geom;
            interaction.set(Some(PreviewInteraction::Drag {
                dx: f64::from(e.client_x()) - x,
                dy: f64::from(e.client_y()) - y,
            }));
        })
    };
    let resize_start = {
        let interaction = interaction.clone();
        Callback::from(move |e: PointerEvent| {
            e.prevent_default();
            capture_pointer(&e);
            interaction.set(Some(PreviewInteraction::Resize));
        })
    };
    let pointer_move = {
        let geom = geom.clone();
        let interaction = interaction.clone();
        Callback::from(move |e: PointerEvent| {
            let (cx, cy) = (f64::from(e.client_x()), f64::from(e.client_y()));
            let (vw, vh) = viewport();
            let (x, y, w, h) = *geom;
            match *interaction {
                Some(PreviewInteraction::Drag { dx, dy }) => {
                    // Keep enough of the bar on-screen to grab again.
                    geom.set((
                        clamp(cx - dx, 8.0 - w + 120.0, vw - 120.0),
                        clamp(cy - dy, 0.0, vh - PREVIEW_BAR_PX),
                        w,
                        h,
                    ));
                }
                Some(PreviewInteraction::Resize) => {
                    geom.set((
                        x,
                        y,
                        clamp(cx - x, 320.0, vw - x - 4.0),
                        clamp(cy - y - PREVIEW_BAR_PX, 160.0, vh - y - PREVIEW_BAR_PX),
                    ));
                }
                None => {}
            }
        })
    };
    let pointer_end = {
        let interaction = interaction.clone();
        Callback::from(move |_: PointerEvent| interaction.set(None))
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
            <div
                class="forward-preview"
                style={format!("left:{}px; top:{}px; width:{}px;", geom.0, geom.1, geom.2)}
            >
                <div class="forward-preview-bar">
                    <button
                        class="forward-preview-btn"
                        title={ if *preview_collapsed { "Expand" } else { "Collapse" } }
                        onclick={toggle_collapse}
                    >
                        { if *preview_collapsed { "▸" } else { "▾" } }
                    </button>
                    // The title span is the drag handle (buttons/links around
                    // it stay plain clicks).
                    <span
                        class="forward-preview-title"
                        onpointerdown={drag_start}
                        onpointermove={pointer_move.clone()}
                        onpointerup={pointer_end.clone()}
                        onpointercancel={pointer_end.clone()}
                    >
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
                // state; only the height changes. Pointer events are disabled
                // during drag/resize so the iframe can't swallow the stream
                // (belt to pointer capture's suspenders).
                <iframe
                    class={classes!(
                        "forward-preview-frame",
                        interaction.is_some().then_some("interacting"),
                    )}
                    style={ if *preview_collapsed {
                        "height:0px;".to_string()
                    } else {
                        format!("height:{}px;", geom.3)
                    }}
                    src={open_url}
                    title="Forwarded app preview"
                />
                if !*preview_collapsed {
                    <div
                        class="forward-preview-grip"
                        title="Resize"
                        onpointerdown={resize_start}
                        onpointermove={pointer_move}
                        onpointerup={pointer_end.clone()}
                        onpointercancel={pointer_end}
                    ></div>
                }
            </div>
        }
        </>
    }
}

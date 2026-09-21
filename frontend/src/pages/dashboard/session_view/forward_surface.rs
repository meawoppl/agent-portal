//! Split-pane host for a session's active port forward.

use shared::api::ForwardInfo;
use uuid::Uuid;
use yew::prelude::*;

use crate::utils;

#[derive(Properties, PartialEq)]
pub struct ForwardSurfaceProps {
    pub session_id: Uuid,
    pub forward: ForwardInfo,
    pub on_close: Callback<()>,
}

#[function_component(ForwardSurface)]
pub fn forward_surface(props: &ForwardSurfaceProps) -> Html {
    let collapsed = use_state(|| false);
    let open_url = utils::api_url(&format!("/api/sessions/{}/forwards/open", props.session_id));
    let title = match &props.forward.process {
        Some(process) => format!("{process} :{} — {}", props.forward.port, props.forward.url),
        None => format!(":{} — {}", props.forward.port, props.forward.url),
    };
    let status_class = match props.forward.listening {
        Some(true) => Some("is-up"),
        Some(false) => Some("is-down"),
        None => None,
    };
    let status_title = match props.forward.listening {
        Some(true) => "Forwarded app is listening",
        Some(false) => "Forwarded port refused the last probe",
        None => "Forward status unknown",
    };
    let toggle_collapsed = {
        let collapsed = collapsed.clone();
        Callback::from(move |_| collapsed.set(!*collapsed))
    };
    let close = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };

    html! {
        <aside class={classes!("session-forward-surface", collapsed.then_some("collapsed"))}>
            <div class="session-forward-toolbar">
                <button
                    type="button"
                    class="surface-icon-button"
                    title={ if *collapsed { "Expand forward" } else { "Collapse forward" } }
                    onclick={toggle_collapsed}
                >
                    { if *collapsed { "▸" } else { "▾" } }
                </button>
                <span class={classes!("surface-status", status_class)} title={status_title}></span>
                <span class="session-forward-title" title={title.clone()}>{ title }</span>
                <a
                    class="session-forward-visit"
                    href={open_url.clone()}
                    target="_blank"
                    rel="noopener noreferrer"
                >
                    { "Visit site ↗" }
                </a>
                <button
                    type="button"
                    class="surface-icon-button surface-close-button"
                    title="Close forward"
                    onclick={close}
                >
                    { "×" }
                </button>
            </div>
            if !*collapsed {
                <iframe
                    class="session-forward-frame"
                    src={open_url}
                    title="Forwarded app"
                />
            }
        </aside>
    }
}

//! The active forward alongside its session, including access and stream details.
use crate::utils;
use gloo_net::http::Request;
use shared::api::{SetForwardPublicRequest, UserForwardInfo};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct SessionForwardProps {
    pub forward: UserForwardInfo,
    pub can_edit: bool,
    pub on_changed: Callback<()>,
}

#[function_component(SessionForward)]
pub(super) fn session_forward(props: &SessionForwardProps) -> Html {
    let saving = use_state(|| false);
    let error = use_state(|| None::<String>);
    let forward = &props.forward;
    let onchange = {
        let saving = saving.clone();
        let error = error.clone();
        let on_changed = props.on_changed.clone();
        let session_id = forward.session_id;
        let public = !forward.public;
        Callback::from(move |_: Event| {
            if *saving {
                return;
            }
            saving.set(true);
            error.set(None);
            let saving = saving.clone();
            let error = error.clone();
            let on_changed = on_changed.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/sessions/{session_id}/forwards/public"));
                match utils::send_json(Request::patch(&url), &SetForwardPublicRequest { public })
                    .await
                {
                    Ok(response) if response.ok() => on_changed.emit(()),
                    _ => error.set(Some("Could not change forward access. Try again.".into())),
                }
                saving.set(false);
            });
        })
    };
    html! {
        <div class="session-forward">
            <a class="forwarding-url" href={forward.url.clone()} target="_blank" rel="noopener noreferrer">
                {format!("{} (:{})", forward.url, forward.port)}
            </a>
            <label class="toggle-label forwarding-public" title="Public forwards are accessible to anyone with the URL">
                <input type="checkbox" checked={forward.public} disabled={!props.can_edit || *saving}
                    aria-label={format!("Public forward for {}", forward.session_name)} {onchange}/>
                <span class={classes!("forwarding-badge", forward.public.then_some("is-public"))}>
                    {if forward.public {"Public"} else {"Private"}}
                </span>
            </label>
            <details class="forwarding-connections">
                <summary>{format!("{} / {} live connections", forward.connections.len(), shared::api::FORWARD_STREAM_LIMIT)}</summary>
                if forward.connections.is_empty() {
                    <span class="forwarding-connection-empty">{"No live tunnel streams"}</span>
                } else {
                    <ul>{for forward.connections.iter().map(|connection| html! {
                        <li key={connection.stream_id.to_string()}>
                            <code>{connection.stream_id.to_string()}</code>
                            {format!(" · :{} · {} · {}", connection.port, connection.transport, connection.opened_at)}
                        </li>
                    })}</ul>
                }
            </details>
            if let Some(error) = &*error { <span class="error-message" role="alert">{error}</span> }
        </div>
    }
}

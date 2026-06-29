// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Agent Messaging page: list your sessions and send a message into one. The
//! message is delivered to that session's agent as an input turn. Backed by
//! `GET /api/agent/sessions` and `POST /api/agent/sessions/{id}/message`.

use gloo_net::http::Request;
use shared::api::{
    AgentSessionInfo, AgentSessionsResponse, SendAgentMessageRequest, SendAgentMessageResponse,
};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlTextAreaElement;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::utils::{self, On401};
use crate::Route;

#[function_component(AgentMessagingPage)]
pub fn agent_messaging_page() -> Html {
    let sessions = use_state(Vec::<AgentSessionInfo>::new);
    let selected = use_state(|| None::<Uuid>);
    let message = use_state(String::new);
    let status = use_state(|| None::<String>);
    let loading = use_state(|| true);

    // Load the caller's sessions once on mount.
    {
        let sessions = sessions.clone();
        let loading = loading.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match utils::fetch_json::<AgentSessionsResponse>(
                    "/api/agent/sessions",
                    On401::Logout,
                )
                .await
                {
                    Ok(resp) => sessions.set(resp.sessions),
                    Err(e) => log::error!("Failed to load sessions: {:?}", e),
                }
                loading.set(false);
            });
            || ()
        });
    }

    let on_message_input = {
        let message = message.clone();
        Callback::from(move |e: InputEvent| {
            message.set(e.target_unchecked_into::<HtmlTextAreaElement>().value());
        })
    };

    let on_send = {
        let selected = selected.clone();
        let message = message.clone();
        let status = status.clone();
        Callback::from(move |_: MouseEvent| {
            let Some(target) = *selected else {
                status.set(Some("Pick a session first.".to_string()));
                return;
            };
            let text = (*message).trim().to_string();
            if text.is_empty() {
                status.set(Some("Type a message first.".to_string()));
                return;
            }
            let message = message.clone();
            let status = status.clone();
            spawn_local(async move {
                let body = SendAgentMessageRequest {
                    message: text,
                    from: None,
                };
                let result = Request::post(&utils::api_url(&format!(
                    "/api/agent/sessions/{}/message",
                    target
                )))
                .json(&body)
                .unwrap()
                .send()
                .await;
                match result {
                    Ok(resp) if resp.ok() => match resp.json::<SendAgentMessageResponse>().await {
                        Ok(r) => {
                            status.set(Some(if r.delivered {
                                format!("Delivered (seq {}).", r.seq)
                            } else {
                                format!("Queued for the session's reconnect (seq {}).", r.seq)
                            }));
                            message.set(String::new());
                        }
                        Err(_) => status.set(Some("Sent, but the response was unreadable.".into())),
                    },
                    Ok(resp) => status.set(Some(format!("Failed: HTTP {}", resp.status()))),
                    Err(e) => status.set(Some(format!("Failed: {}", e))),
                }
            });
        })
    };

    html! {
        <div class="agent-messaging-page" style="max-width:760px; margin:0 auto; padding:1rem;">
            <div style="display:flex; align-items:center; gap:0.75rem; margin-bottom:1rem;">
                <Link<Route> to={Route::Dashboard} classes="back-link">{ "← Dashboard" }</Link<Route>>
                <h1 style="margin:0; font-size:1.2rem;">{ "Agent Messaging" }</h1>
            </div>
            <p style="color:var(--text-secondary); margin-bottom:1rem;">
                { "Send a message into one of your sessions — it arrives as an input turn to that session's agent." }
            </p>

            {
                if *loading {
                    html! { <p>{ "Loading sessions\u{2026}" }</p> }
                } else if sessions.is_empty() {
                    html! { <p style="color:var(--text-muted);">{ "No sessions found." }</p> }
                } else {
                    html! {
                        <div style="display:flex; flex-direction:column; gap:0.4rem; margin-bottom:1rem;">
                            { for sessions.iter().map(|s| {
                                let id = s.id;
                                let is_sel = *selected == Some(id);
                                let selected = selected.clone();
                                let onclick = Callback::from(move |_| selected.set(Some(id)));
                                let border = if is_sel { "var(--accent)" } else { "var(--border)" };
                                html! {
                                    <button type="button" {onclick}
                                        style={format!("text-align:left; padding:0.5rem 0.7rem; border-radius:4px; border:1px solid {}; background:rgba(0,0,0,0.2); color:var(--text-primary); cursor:pointer;", border)}>
                                        <div style="font-weight:600;">{ &s.session_name }</div>
                                        <div style="font-size:0.75rem; color:var(--text-secondary);">
                                            { format!("{} \u{00b7} {} \u{00b7} {} \u{00b7} {}", s.agent_type, s.status, s.hostname, s.working_directory) }
                                        </div>
                                    </button>
                                }
                            }) }
                        </div>
                    }
                }
            }

            <textarea
                placeholder="Message to send\u{2026}"
                value={(*message).clone()}
                oninput={on_message_input}
                rows="4"
                style="width:100%; padding:0.6rem; border-radius:4px; border:1px solid var(--border); background:var(--bg-darker); color:var(--text-primary); font-family:inherit; resize:vertical;"
            />
            <div style="display:flex; align-items:center; gap:0.75rem; margin-top:0.6rem;">
                <button type="button" onclick={on_send}
                    style="padding:0.5rem 1rem; border-radius:4px; border:none; background:var(--accent); color:var(--bg-dark); font-weight:600; cursor:pointer;">
                    { "Send" }
                </button>
                {
                    if let Some(msg) = &*status {
                        html! { <span style="color:var(--text-secondary); font-size:0.85rem;">{ msg }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    }
}

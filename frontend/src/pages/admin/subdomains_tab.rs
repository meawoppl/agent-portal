// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Admin ▸ Subdomains: assign a human-readable custom subdomain to a session's
//! forward (docs/PORT_FORWARDING.md). The custom URL routes alongside the auto
//! hash URL. Deconfliction errors from the backend are shown inline.

use gloo_net::http::Request;
use shared::api::{
    AdminForwardInfo, AdminForwardsResponse, CreateCustomSubdomainRequest, CustomSubdomainInfo,
    CustomSubdomainsResponse,
};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::utils::{self, On401};

#[function_component(AdminSubdomainsTab)]
pub fn admin_subdomains_tab() -> Html {
    let subdomains = use_state(Vec::<CustomSubdomainInfo>::new);
    let forwards = use_state(Vec::<AdminForwardInfo>::new);
    let selected_session = use_state(|| None::<Uuid>);
    let label_input = use_state(String::new);
    let error = use_state(|| None::<String>);
    let saving = use_state(|| false);

    let reload = {
        let subdomains = subdomains.clone();
        let forwards = forwards.clone();
        move || {
            let subdomains = subdomains.clone();
            let forwards = forwards.clone();
            spawn_local(async move {
                if let Ok(d) = utils::fetch_json::<CustomSubdomainsResponse>(
                    "/api/admin/subdomains",
                    On401::Ignore,
                )
                .await
                {
                    subdomains.set(d.subdomains);
                }
                if let Ok(d) =
                    utils::fetch_json::<AdminForwardsResponse>("/api/admin/forwards", On401::Ignore)
                        .await
                {
                    forwards.set(d.forwards);
                }
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

    let on_session_change = {
        let selected_session = selected_session.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            selected_session.set(Uuid::parse_str(&select.value()).ok());
        })
    };

    let on_label_input = {
        let label_input = label_input.clone();
        let error = error.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            label_input.set(input.value());
            error.set(None);
        })
    };

    let on_save = {
        let reload = reload.clone();
        let selected_session = selected_session.clone();
        let label_input = label_input.clone();
        let error = error.clone();
        let saving = saving.clone();
        Callback::from(move |_: MouseEvent| {
            let Some(session_id) = *selected_session else {
                error.set(Some("Pick a session with an active forward.".to_string()));
                return;
            };
            let label = (*label_input).trim().to_string();
            if label.is_empty() {
                error.set(Some("Enter a subdomain.".to_string()));
                return;
            }
            let reload = reload.clone();
            let label_input = label_input.clone();
            let error = error.clone();
            let saving = saving.clone();
            spawn_local(async move {
                saving.set(true);
                let url = utils::api_url("/api/admin/subdomains");
                let body = CreateCustomSubdomainRequest { session_id, label };
                match Request::post(&url).json(&body).unwrap().send().await {
                    Ok(resp) if resp.status() == 201 => {
                        label_input.set(String::new());
                        error.set(None);
                        reload();
                    }
                    Ok(resp) => {
                        // Deconfliction / validation message from the backend.
                        let msg = resp.text().await.unwrap_or_default();
                        error.set(Some(if msg.is_empty() {
                            format!("Failed (HTTP {})", resp.status())
                        } else {
                            msg
                        }));
                    }
                    Err(e) => error.set(Some(format!("Request failed: {e}"))),
                }
                saving.set(false);
            });
        })
    };

    let on_delete = {
        let reload = reload.clone();
        Callback::from(move |label: String| {
            let reload = reload.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/admin/subdomains/{label}"));
                let _ = Request::delete(&url).send().await;
                reload();
            });
        })
    };

    let domain_suffix = forwards
        .first()
        .and_then(|f| f.url.split_once("://").map(|(_, rest)| rest))
        .and_then(|host_path| host_path.split_once('.').map(|(_, d)| d))
        .map(|d| d.trim_end_matches('/').to_string())
        .unwrap_or_default();

    html! {
        <section class="admin-subdomains">
            <div class="section-header">
                <h2>{ "Custom Subdomains" }</h2>
                <p class="section-description">
                    { "Give a session's forward a friendly subdomain. It routes \
                       alongside the auto-generated URL." }
                </p>
            </div>

            <div class="subdomain-create">
                <select onchange={on_session_change}>
                    <option value="" selected={selected_session.is_none()}>
                        { "Select a forwarded session…" }
                    </option>
                    { for forwards.iter().map(|f| html! {
                        <option value={f.session_id.to_string()}>
                            { format!("{} — :{} ({})", f.session_name, f.port, f.owner_email) }
                        </option>
                    }) }
                </select>
                <div class="subdomain-input-group">
                    <input
                        type="text"
                        placeholder="my-app"
                        value={(*label_input).clone()}
                        oninput={on_label_input}
                    />
                    if !domain_suffix.is_empty() {
                        <span class="subdomain-suffix">{ format!(".{domain_suffix}") }</span>
                    }
                </div>
                <button
                    class="create-button"
                    onclick={on_save}
                    disabled={*saving || forwards.is_empty()}
                >
                    { if *saving { "Saving…" } else { "Assign" } }
                </button>
            </div>
            if let Some(err) = (*error).clone() {
                <p class="subdomain-error">{ err }</p>
            }
            if forwards.is_empty() {
                <p class="empty-state">{ "No active forwards to assign a subdomain to." }</p>
            }

            if !subdomains.is_empty() {
                <table class="admin-table subdomain-table">
                    <thead>
                        <tr>
                            <th>{ "Subdomain" }</th>
                            <th>{ "Session" }</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>
                        { for subdomains.iter().map(|s| {
                            let label = s.label.clone();
                            let on_delete = on_delete.clone();
                            html! {
                                <tr key={s.label.clone()}>
                                    <td>
                                        <a href={s.url.clone()} target="_blank" rel="noopener noreferrer">
                                            { &s.label }
                                        </a>
                                    </td>
                                    <td>{ &s.session_name }</td>
                                    <td>
                                        <button
                                            class="delete-btn"
                                            onclick={Callback::from(move |_| on_delete.emit(label.clone()))}
                                        >{ "Remove" }</button>
                                    </td>
                                </tr>
                            }
                        }) }
                    </tbody>
                </table>
            }
        </section>
    }
}

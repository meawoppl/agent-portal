use crate::federation::{self, Federation, FederationAction};
use gloo_net::http::Request;
use shared::api::{
    DeviceCodeResponse, DeviceFlowPollRequest, LinkedClientEnrollmentRequest,
    LinkedClientPollResponse, PortalConnection, TokenScope, UpsertPortalConnectionRequest,
    CAP_LINKED_CLIENT_WRITE,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, RequestCredentials, RequestRedirect};
use yew::prelude::*;

#[derive(Clone, PartialEq)]
struct Pending {
    origin: String,
    instance: uuid::Uuid,
    device_code: String,
    user_code: String,
    interval: u32,
    sync: bool,
    scope: TokenScope,
}

fn request(origin: &str, path: &str) -> gloo_net::http::RequestBuilder {
    Request::post(&format!("{origin}{path}"))
        .credentials(RequestCredentials::Omit)
        .redirect(RequestRedirect::Error)
}

#[function_component(LinkedPortals)]
pub fn linked_portals() -> Html {
    let Some(registry) = use_context::<Federation>() else {
        return html! { <p>{"Connection context unavailable"}</p> };
    };
    let origin = use_state(String::new);
    let write = use_state(|| true);
    let sync = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(String::new);
    let pending = use_state(|| None::<Pending>);
    let on_origin = {
        let origin = origin.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(input) = event
                .target()
                .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            {
                origin.set(input.value());
            }
        })
    };
    let on_write = {
        let write = write.clone();
        Callback::from(move |event: Event| {
            if let Some(input) = event
                .target()
                .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            {
                write.set(input.checked());
            }
        })
    };
    let on_sync = {
        let sync = sync.clone();
        Callback::from(move |event: Event| {
            if let Some(input) = event
                .target()
                .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            {
                sync.set(input.checked());
            }
        })
    };
    let begin = {
        let (origin, write, sync, busy, error, pending, registry) = (
            origin.clone(),
            write.clone(),
            sync.clone(),
            busy.clone(),
            error.clone(),
            pending.clone(),
            registry.clone(),
        );
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            if *busy || registry.home.account_id().is_none() {
                return;
            }
            let canonical = match shared::urls::canonical_origin(origin.trim()) {
                Ok(o) => o,
                Err(e) => {
                    error.set(e.to_string());
                    return;
                }
            };
            if canonical == registry.home.origin() {
                error.set("This is already your home portal".into());
                return;
            }
            let scope = if *write {
                TokenScope::Write
            } else {
                TokenScope::Read
            };
            let (sync, busy, error, pending, home) = (
                *sync,
                busy.clone(),
                error.clone(),
                pending.clone(),
                registry.home.clone(),
            );
            busy.set(true);
            error.set(String::new());
            spawn_local(async move {
                let result = async {
                    let discovery = federation::discover(&canonical).await?;
                    if scope == TokenScope::Write && !discovery.capabilities.iter().any(|c| c == CAP_LINKED_CLIENT_WRITE) {
                        return Err("This portal does not support linked write access; choose read-only".into());
                    }
                    let response = request(&canonical, "/api/auth/linked/code").json(&LinkedClientEnrollmentRequest {
                        client_origin: home.origin().into(), requested_scope: scope, device_label: Some("Portal browser tab".into()),
                    }).map_err(|e| e.to_string())?.send().await.map_err(|e| e.to_string())?;
                    if !response.ok() { return Err(format!("Enrollment refused (HTTP {}). The remote administrator may need to enable linking or allow this origin.", response.status())); }
                    let code: DeviceCodeResponse = response.json().await.map_err(|e| e.to_string())?;
                    if code.user_code.len() > 64 || !code.user_code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                        return Err("The portal returned an invalid approval code".into());
                    }
                    Ok(Pending { origin: canonical, instance: discovery.instance_id, device_code: code.device_code,
                        user_code: code.user_code, interval: code.interval.clamp(1,30) as u32 * 1000, sync, scope })
                }.await;
                match result {
                    Ok(value) => pending.set(Some(value)),
                    Err(e) => error.set(e),
                }
                busy.set(false);
            });
        })
    };
    {
        let (registry, error, pending_state) = (registry.clone(), error.clone(), pending.clone());
        use_effect_with((*pending).clone(), move |pending| {
            let cancelled = Rc::new(Cell::new(false));
            if let Some(pending) = pending.clone() {
                let guard = cancelled.clone();
                spawn_local(async move {
                    // Enrollment is bounded to ten minutes even if a broken
                    // peer keeps returning Pending forever.
                    let deadline = js_sys::Date::now() + 600_000.0;
                    loop {
                        gloo::timers::future::TimeoutFuture::new(pending.interval).await;
                        if guard.get() {
                            return;
                        }
                        if js_sys::Date::now() >= deadline {
                            error.set("Approval expired; start again".into());
                            pending_state.set(None);
                            return;
                        }
                        let result = async {
                            let response = request(&pending.origin, "/api/auth/linked/poll")
                                .json(&DeviceFlowPollRequest {
                                    device_code: pending.device_code.clone(),
                                })
                                .map_err(|e| e.to_string())?
                                .send()
                                .await
                                .map_err(|e| e.to_string())?;
                            if !response.ok() {
                                return Err(format!("Polling failed (HTTP {})", response.status()));
                            }
                            response
                                .json::<LinkedClientPollResponse>()
                                .await
                                .map_err(|e| e.to_string())
                        }
                        .await;
                        if guard.get() {
                            return;
                        }
                        match result {
                            Ok(LinkedClientPollResponse::Pending) => continue,
                            Ok(LinkedClientPollResponse::Complete {
                                access_token,
                                expires_at,
                                portal_origin,
                                instance_id,
                                grant,
                            }) => {
                                if portal_origin != pending.origin
                                    || instance_id != pending.instance
                                    || grant.client_origin != registry.home.origin()
                                    || grant.scope != pending.scope
                                    || grant.revoked_at.is_some()
                                {
                                    error.set("The approval did not match the requested portal, instance, origin or scope".into());
                                    pending_state.set(None);
                                    return;
                                }
                                let mut synced_id = None;
                                if pending.sync {
                                    let metadata = UpsertPortalConnectionRequest {
                                        portal_origin: portal_origin.clone(),
                                        instance_id,
                                        remote_user_id: grant.user_id,
                                        remote_email: grant.user_email.clone(),
                                        label: portal_origin.clone(),
                                        scope: grant.scope,
                                    };
                                    // This is the only request to home: metadata,
                                    // never the device code or access credential.
                                    let saved = async {
                                        let response = registry
                                            .home
                                            .post("/api/portal-connections")
                                            .json(&metadata)
                                            .map_err(|e| e.to_string())?
                                            .send()
                                            .await
                                            .map_err(|e| e.to_string())?;
                                        if !response.ok() {
                                            return Err(format!("HTTP {}", response.status()));
                                        }
                                        response
                                            .json::<PortalConnection>()
                                            .await
                                            .map_err(|e| e.to_string())
                                    }
                                    .await;
                                    if guard.get() {
                                        return;
                                    }
                                    match saved {
                                        Ok(row) => synced_id = Some(row.id),
                                        Err(e) => error.set(format!(
                                            "Connected in this tab, but metadata sync failed: {e}"
                                        )),
                                    }
                                }
                                registry.dispatch(FederationAction::Add {
                                    origin: portal_origin,
                                    instance: instance_id,
                                    grant,
                                    token: access_token,
                                    expiry: expires_at,
                                    synced_id,
                                });
                                pending_state.set(None);
                                return;
                            }
                            Ok(LinkedClientPollResponse::Denied { reason }) => {
                                error.set(format!("Approval denied: {reason:?}"));
                            }
                            Ok(LinkedClientPollResponse::Expired) => {
                                error.set("Approval expired; start again".into())
                            }
                            Err(e) => error.set(e),
                        }
                        pending_state.set(None);
                        return;
                    }
                });
            }
            move || cancelled.set(true)
        });
    }
    let cancel = {
        let pending = pending.clone();
        Callback::from(move |_| pending.set(None))
    };
    html! {
        <section class="settings-panel linked-portals">
            <h2>{"Linked portals"}</h2>
            <p>{"Use sessions from another portal without merging your accounts. Credentials stay only in this tab; closing or reloading it requires authorization again. Use Revoke and unlink before closing to revoke remote access."}</p>
            <p>{"Only link portals you trust. The home portal's frontend can access linked credentials. Syncing the connection list reveals account and portal metadata to the home operator."}</p>
            if !error.is_empty() { <p role="alert" class="error">{(*error).clone()}</p> }
            if let Some(pending) = &*pending {
                <p>{format!("Approve on {} using code {}", pending.origin, pending.user_code)}</p>
                <a href={format!("{}/api/auth/linked?user_code={}", pending.origin, pending.user_code)} target="_blank" rel="noopener noreferrer">{"Open approval page"}</a>
                <button onclick={cancel}>{"Cancel"}</button>
            } else {
                <form onsubmit={begin}>
                    <label>{"Portal origin"}<input type="url" required=true placeholder="https://portal.example" value={(*origin).clone()} oninput={on_origin} disabled={*busy}/></label>
                    <label><input type="checkbox" checked={*write} onchange={on_write} disabled={*busy}/>{"Allow commands and edits (uncheck for read-only)"}</label>
                    <label><input type="checkbox" checked={*sync} onchange={on_sync} disabled={*busy}/>{"Sync connection metadata to my home account"}</label>
                    <button type="submit" disabled={*busy || registry.home.account_id().is_none()}>{if *busy {"Checking portal…"} else {"Link portal"}}</button>
                </form>
            }
            { for registry.rows.iter().map(|row| {
                let (row, registry, error) = (row.clone(), registry.clone(), error.clone());
                let action_row = row.clone();
                let remove = Callback::from(move |_| {
                    let (row, registry, error) = (action_row.clone(), registry.clone(), error.clone());
                    spawn_local(async move {
                        if let Some(client) = &row.client { if let Err(e) = client.revoke_link().await {
                            error.set(format!("Remote revocation failed: {e}. Open the remote portal to revoke this grant.")); return;
                        } }
                        if row.client.is_some() { registry.dispatch(FederationAction::Deactivate(row.id, "Remote grant revoked; removing saved metadata".into())); }
                        if let Some(id) = row.synced_id {
                            match registry.home.delete(&format!("/api/portal-connections/{id}")).send().await {
                                Ok(response) if response.ok() => {},
                                _ => { error.set("Could not remove saved connection metadata".into()); return; }
                            }
                        }
                        registry.dispatch(FederationAction::Remove(row.id));
                    });
                });
                html! { <article class="linked-portal-card">
                    <strong>{&row.origin}</strong><p>{&row.email}</p><p>{&row.status}</p>
                    <button onclick={remove}>{if row.client.is_some() {"Revoke and unlink"} else {"Remove saved connection"}}</button>
                </article> }
            }) }
            <IncomingGrants />
        </section>
    }
}

/// Native account management remains available after a linked tab is lost.
/// This list never uses a linked bearer and cannot enumerate another portal.
#[function_component(IncomingGrants)]
fn incoming_grants() -> Html {
    let portal = crate::portal_client::use_portal();
    let grants = use_state(Vec::<shared::api::LinkedClientGrant>::new);
    let error = use_state(String::new);
    let revision = use_state(|| 0u64);
    {
        let (grants, error) = (grants.clone(), error.clone());
        use_effect_with((portal.clone(), *revision), move |(portal, _)| {
            let cancelled = Rc::new(Cell::new(false));
            let guard = cancelled.clone();
            let portal = portal.clone();
            spawn_local(async move {
                let result = portal
                    .fetch_json::<shared::api::LinkedClientGrantsResponse>(
                        "/api/linked-grants",
                        crate::utils::On401::Ignore,
                    )
                    .await;
                if !guard.get() {
                    match result {
                        Ok(list) => grants.set(list.grants),
                        Err(e) => error.set(format!("Cannot load grants: {e}")),
                    }
                }
            });
            move || cancelled.set(true)
        });
    }
    html! {
        <section class="incoming-linked-grants">
            <h3>{"Portals with access to this account"}</h3>
            <p>{"Revoke access here if you closed a linked tab or no longer trust its home portal."}</p>
            if !error.is_empty() { <p role="alert">{&*error}</p> }
            { for grants.iter().filter(|g| g.revoked_at.is_none()).map(|grant| {
                let (portal, error, revision, id) = (portal.clone(), error.clone(), revision.clone(), grant.id);
                let revoke = Callback::from(move |_| {
                    let (portal, error, revision) = (portal.clone(), error.clone(), revision.clone());
                    spawn_local(async move {
                        match portal.delete(&format!("/api/linked-grants/{id}")).send().await {
                            Ok(response) if response.status() == 204 => revision.set(*revision + 1),
                            _ => error.set("Revocation failed; try again".into()),
                        }
                    });
                });
                html! { <article class="linked-portal-card">
                    <strong>{&grant.client_origin}</strong>
                    <p>{format!("{} · {} · expires {}", grant.device_label.as_deref().unwrap_or("Linked browser"), grant.scope.as_str(), grant.expires_at)}</p>
                    <button onclick={revoke}>{"Revoke access"}</button>
                </article> }
            }) }
        </section>
    }
}

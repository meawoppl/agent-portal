//! Bounded, admin-only history; filter changes discard stale in-flight results.
use crate::utils::{self, On401};
use shared::audit::{AuditEvent, AuditPage};
use std::{cell::Cell, rc::Rc};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlSelectElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub users: Vec<super::AdminUserInfo>,
}
#[function_component(AdminAuditTab)]
pub fn audit_tab(props: &Props) -> Html {
    let category = use_state(String::new);
    let actor = use_state(String::new);
    let before = use_state(|| None::<(String, Uuid)>);
    let reload = use_state(|| 0u32);
    let page = use_state(|| None::<AuditPage>);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    {
        let page = page.clone();
        let loading = loading.clone();
        let error = error.clone();
        use_effect_with(
            (
                (*category).clone(),
                (*actor).clone(),
                (*before).clone(),
                *reload,
            ),
            move |(category, actor, before, _)| {
                let mut url = "/api/admin/audit?limit=50".to_string();
                if !category.is_empty() {
                    url.push_str(&format!("&category={category}"));
                }
                if !actor.is_empty() {
                    url.push_str(&format!("&actor={actor}"));
                }
                if let Some((time, id)) = before {
                    url.push_str(&format!(
                        "&before_time={}&before_id={id}",
                        js_sys::encode_uri_component(time)
                    ));
                }
                let cancelled = Rc::new(Cell::new(false));
                let guard = cancelled.clone();
                loading.set(true);
                error.set(None);
                page.set(None);
                spawn_local(async move {
                    let result = utils::fetch_json::<AuditPage>(&url, On401::Ignore).await;
                    if guard.get() {
                        return;
                    }
                    match result {
                        Ok(value) => page.set(Some(value)),
                        Err(e) => error.set(Some(format!("Could not load audit history: {e}"))),
                    }
                    loading.set(false);
                });
                move || cancelled.set(true)
            },
        );
    }
    let category_change = {
        let category = category.clone();
        let before = before.clone();
        Callback::from(move |event: Event| {
            category.set(event.target_unchecked_into::<HtmlSelectElement>().value());
            before.set(None);
        })
    };
    let actor_change = {
        let actor = actor.clone();
        let before = before.clone();
        Callback::from(move |event: Event| {
            actor.set(event.target_unchecked_into::<HtmlSelectElement>().value());
            before.set(None);
        })
    };
    let refresh = {
        let before = before.clone();
        let reload = reload.clone();
        Callback::from(move |_| {
            before.set(None);
            reload.set(*reload + 1);
        })
    };
    let older = {
        let page = page.clone();
        let before = before.clone();
        Callback::from(move |_| {
            if let Some(last) = page.as_ref().and_then(|p| p.events.last()) {
                before.set(Some((last.occurred_at.clone(), last.id)));
            }
        })
    };
    html! {
        <section class="admin-audit" aria-label="Audit log">
            <h2>{"Audit log"}</h2>
            <p class="audit-description">{"Login and identity, computers, and secrets. Credential values and tokens are never recorded."}</p>
            <div class="audit-controls">
                <label>{"Category"}<select aria-label="Category" value={(*category).clone()} onchange={category_change}>
                    <option value="">{"All categories"}</option><option value="identity">{"Login / identity"}</option>
                    <option value="computers">{"Computers"}</option><option value="secrets">{"Secrets"}</option>
                </select></label>
                <label>{"Actor"}<select aria-label="Actor" value={(*actor).clone()} onchange={actor_change}>
                    <option value="">{"All actors"}</option>
                    {for props.users.iter().map(|u| html! {<option value={u.id.to_string()}>{&u.email}</option>})}
                </select></label>
                <button class="header-button" onclick={refresh} disabled={*loading}>{"Latest events"}</button>
            </div>
            if *loading { <p role="status">{"Loading audit history…"}</p> }
            if let Some(message) = &*error { <p role="alert">{message}</p> }
            if let Some(data) = &*page {
                <p class="audit-description">{match data.retention_days { Some(days) => format!("Retention: {days} days · Newest first · Times in UTC"), None => "Retention: unlimited · Newest first · Times in UTC".into() }}</p>
                if data.events.is_empty() { <p>{"No events match these filters."}</p> }
                <div class="audit-events">
                    {for data.events.iter().map(render_event)}
                </div>
                <button class="header-button" onclick={older} disabled={!data.has_more || *loading}>{"Older events"}</button>
            }
        </section>
    }
}
fn render_event(event: &AuditEvent) -> Html {
    let actor = event
        .actor_email
        .clone()
        .unwrap_or_else(|| match event.actor_kind.as_str() {
            "system" => "System".into(),
            "unknown" => "Unauthenticated / unknown".into(),
            _ => "Former user".into(),
        });
    let title = event.event_type.replace(['.', '_'], " ");
    let ids = [
        ("Actor", event.actor_user_id),
        ("Account", event.owner_user_id),
        ("Resource", event.target_id),
        ("Session", event.session_id),
        ("Computer", event.launcher_id),
        ("Operation", event.operation_id),
    ];
    html! {
        <details class="audit-event" key={event.id.to_string()}>
            <summary>
                <time datetime={event.occurred_at.clone()}>{event.occurred_at.replace('T'," ").split('.').next().unwrap_or(&event.occurred_at)}</time>
                <span class="audit-action">{title}</span>
                <span>{actor}{if event.actor_kind == "agent" { " · Agent" } else { "" }}</span>
                <span class="audit-outcome">{&event.outcome}</span>
            </summary>
            <dl>
                <dt>{"Resource type"}</dt><dd>{&event.target_kind}</dd>
                {for ids.into_iter().filter_map(|(label,value)| value.map(|id|html!{<><dt>{label}</dt><dd>{id.to_string()}</dd></>}))}
                if let Some(grantee) = event.details.grantee_user_id {
                    <dt>{"Recipient"}</dt><dd>{grantee.to_string()}</dd>
                    <dt>{"Permissions"}</dt><dd>{format!("Read: {} · Use: {} · Write: {} · Manage: {}",event.details.can_read.unwrap_or(false),event.details.can_use.unwrap_or(false),event.details.can_write.unwrap_or(false),event.details.can_manage.unwrap_or(false))}</dd>
                }
                if let Some(provider) = &event.details.provider { <dt>{"Provider"}</dt><dd>{provider}</dd> }
                if let Some(version) = event.details.version { <dt>{"Version"}</dt><dd>{version}</dd> }
            </dl>
        </details>
    }
}

//! Session-scoped plugin picker. Saving changes future launches, never a host install.
use crate::portal_client::use_portal;
use gloo::events::EventListener;
use shared::api::{PluginInventoryResponse, SetSessionPluginRequest};
use std::{cell::Cell, rc::Rc};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlElement, HtmlInputElement, KeyboardEvent, Node};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct PluginSelectorProps {
    pub session_id: Uuid,
    pub is_owner: bool,
    pub on_loaded: Callback<(Uuid, PluginInventoryResponse)>,
    pub on_details: Callback<MouseEvent>,
}

#[function_component(PluginSelector)]
pub(super) fn plugin_selector(props: &PluginSelectorProps) -> Html {
    let portal = use_portal();
    let inventory = use_state(PluginInventoryResponse::default);
    let busy = use_state(|| true);
    let error = use_state(|| None::<String>);
    let saved = use_state(|| false);
    let refresh = use_state(|| 0u32);
    let root = use_node_ref();
    // Parent keys this component by session, so unmount cancels pending writes.
    let alive = use_mut_ref(|| true);
    {
        let alive = alive.clone();
        use_effect_with((), move |_| move || *alive.borrow_mut() = false);
    }
    {
        let root = root.clone();
        use_effect_with((), move |_| {
            let outside_root = root.clone();
            let click = EventListener::new(&gloo::utils::document(), "click", move |event| {
                if let Some(element) = outside_root.cast::<HtmlElement>() {
                    let target = event
                        .target()
                        .and_then(|target| target.dyn_into::<Node>().ok());
                    if !element.contains(target.as_ref()) {
                        let _ = element.remove_attribute("open");
                    }
                }
            });
            let key = EventListener::new(&gloo::utils::document(), "keydown", move |event| {
                if event
                    .dyn_ref::<KeyboardEvent>()
                    .is_some_and(|event| event.key() == "Escape")
                {
                    if let Some(element) = root.cast::<HtmlElement>() {
                        if element.has_attribute("open") {
                            let _ = element.remove_attribute("open");
                            if let Ok(Some(summary)) = element.query_selector("summary") {
                                if let Some(summary) = summary.dyn_ref::<HtmlElement>() {
                                    let _ = summary.focus();
                                }
                            }
                        }
                    }
                }
            });
            move || {
                drop(click);
                drop(key);
            }
        });
    }
    {
        let inventory = inventory.clone();
        let busy = busy.clone();
        let error = error.clone();
        let on_loaded = props.on_loaded.clone();
        let session_id = props.session_id;
        let portal = portal.clone();
        use_effect_with(*refresh, move |_| {
            let cancelled = Rc::new(Cell::new(false));
            let guard = cancelled.clone();
            busy.set(true);
            spawn_local(async move {
                let result = async {
                    let response = portal
                        .get(&format!("/api/plugins?sessionId={session_id}"))
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    if !response.ok() {
                        return Err(response
                            .text()
                            .await
                            .unwrap_or_else(|_| "Could not load plugins".into()));
                    }
                    response
                        .json::<PluginInventoryResponse>()
                        .await
                        .map_err(|e| e.to_string())
                }
                .await;
                if !guard.get() {
                    match result {
                        Ok(data) => {
                            on_loaded.emit((session_id, data.clone()));
                            inventory.set(data);
                            error.set(None);
                        }
                        Err(message) => error.set(Some(message)),
                    }
                    busy.set(false);
                }
            });
            move || cancelled.set(true)
        });
    }
    let can_edit = props.is_owner && inventory.session_selection_supported && !*busy;
    let count = inventory.plugins.iter().filter(|p| p.enabled).count();
    let on_refresh = {
        let refresh = refresh.clone();
        Callback::from(move |_| refresh.set(*refresh + 1))
    };
    let details = {
        let root = root.clone();
        let on_details = props.on_details.clone();
        Callback::from(move |event| {
            if let Some(element) = root.cast::<HtmlElement>() {
                let _ = element.remove_attribute("open");
            }
            on_details.emit(event);
        })
    };
    html! {
        <details ref={root} class="plugin-selector">
            <summary class="session-header-action plugin-action" title="Choose plugins for this session">{format!("Plugins {count} ▾")}</summary>
            <div class="plugin-selector-menu" aria-label="Session plugins" aria-busy={busy.to_string()}>
                <strong>{"Plugins for this session"}</strong>
                <p>{"Choose installed plugin skills. These are the skills the next launch loads; unchosen plugins are on where their files are detected. Changes apply after restarting this session."}</p>
                if !props.is_owner { <p>{"Only the session owner can change this selection."}</p> }
                if let Some(message) = &*error { <p role="alert" class="plugin-selector-error">{message}</p> }
                if *busy { <p role="status">{"Loading / saving…"}</p> }
                <div class="plugin-selector-options">
                    {for inventory.plugins.iter().map(|plugin| {
                        let name = plugin.name.clone();
                        let portal = portal.clone(); let busy = busy.clone(); let error = error.clone();
                        let inventory = inventory.clone(); let saved = saved.clone(); let alive = alive.clone();
                        let on_loaded = props.on_loaded.clone(); let session_id = props.session_id;
                        let onchange = Callback::from(move |event: Event| {
                            let enabled = event.target_unchecked_into::<HtmlInputElement>().checked();
                            let request = SetSessionPluginRequest {session_id, name: name.clone(), enabled};
                            let portal = portal.clone(); let busy = busy.clone(); let error = error.clone();
                            let inventory = inventory.clone(); let saved = saved.clone(); let alive = alive.clone(); let on_loaded = on_loaded.clone();
                            let previous = (*inventory).clone();
                            let mut optimistic = previous.clone();
                            if let Some(plugin) = optimistic.plugins.iter_mut().find(|p| p.name == name) {
                                plugin.enabled = enabled;
                            }
                            inventory.set(optimistic);
                            busy.set(true); error.set(None); saved.set(false);
                            spawn_local(async move {
                                let result = async {
                                    let response = portal.patch("/api/plugins").json(&request).map_err(|e| e.to_string())?.send().await.map_err(|e| e.to_string())?;
                                    if !response.ok() { return Err(response.text().await.unwrap_or_else(|_| "Could not save selection".into())); }
                                    response.json::<PluginInventoryResponse>().await.map_err(|e| e.to_string())
                                }.await;
                                if *alive.borrow() {
                                    match result {
                                        Ok(data) => { on_loaded.emit((session_id, data.clone())); inventory.set(data); saved.set(true); },
                                        Err(message) => { inventory.set(previous); error.set(Some(message)); },
                                    }
                                    busy.set(false);
                                }
                            });
                        });
                        html! { <label class="plugin-selector-option" key={plugin.name.clone()}>
                            <input type="checkbox" checked={plugin.enabled} disabled={!can_edit} {onchange}/>
                            <span><b>{&plugin.display_name}</b>
                                if let Some(description) = &plugin.description { <small>{description}</small> }
                                if plugin.suggested { <small>{"Suggested for this project"}</small> }
                                {for plugin.warnings.iter().map(|warning| html! {<small class="plugin-selector-error">{warning}</small>})}
                            </span>
                        </label> }
                    })}
                </div>
                if !*busy && inventory.plugins.is_empty() && error.is_none() { <p>{"No plugins installed on this session’s machine."}</p> }
                if *saved { <p role="status">{"Saved. Restart this session to apply."}</p> }
                <div class="plugin-selector-actions">
                    <button type="button" class="session-header-action" onclick={on_refresh} disabled={*busy}>{"Refresh"}</button>
                    <button type="button" class="session-header-action" onclick={details} disabled={*busy}>{"Plugin details"}</button>
                </div>
            </div>
        </details>
    }
}

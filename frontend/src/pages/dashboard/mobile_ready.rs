use super::PortalSession;
use crate::portal_client::SessionKey;
use gloo::{events::EventListener, timers::callback::Timeout};
use shared::api::SessionAttention;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::JsCast;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct Props {
    pub sessions: Vec<PortalSession>,
    pub rows: HashMap<SessionKey, SessionAttention>,
    pub hidden: HashSet<SessionKey>,
    pub focused: Option<SessionKey>,
    pub unavailable: bool,
    pub mutation_failed: bool,
    pub toast: Option<SessionKey>,
    pub on_dismiss: Callback<()>,
    pub on_select: Callback<usize>,
    pub on_mute: Callback<(SessionKey, bool)>,
    pub on_mark_read: Callback<(SessionKey, uuid::Uuid)>,
    pub on_settings: Callback<MouseEvent>,
}
#[function_component(MobileReady)]
pub(super) fn mobile_ready(props: &Props) -> Html {
    let open = use_state(|| false);
    let ready_only = use_state(|| false);
    let search = use_state(String::new);
    let trigger = use_node_ref();
    let sheet = use_node_ref();
    let close = {
        let open = open.clone();
        let trigger = trigger.clone();
        Callback::from(move |()| {
            open.set(false);
            if let Some(el) = trigger.cast::<web_sys::HtmlElement>() {
                let _ = el.focus();
            }
        })
    };
    {
        let close = close.clone();
        let sheet = sheet.clone();
        use_effect_with(*open, move |open| {
            if *open {
                if let Some(el) = sheet.cast::<web_sys::HtmlElement>() {
                    let _ = el.focus();
                }
            }
            let listener = if *open {
                web_sys::window().map(|w| {
                    EventListener::new(&w, "keydown", move |event| {
                        if let Some(event) = event.dyn_ref::<web_sys::KeyboardEvent>() {
                            if event.key() == "Escape" {
                                event.prevent_default();
                                close.emit(());
                            }
                            // Keep keyboard navigation inside the modal on tablets and desktop emulation.
                            if event.key() == "Tab" {
                                if let Some(el) = sheet.cast::<web_sys::Element>() {
                                    if let Ok(nodes) = el.query_selector_all(
                                        "button:not([disabled]), input, a[href]",
                                    ) {
                                        let len = nodes.length();
                                        if len > 0 {
                                            let first = nodes.item(0).and_then(|n| {
                                                n.dyn_into::<web_sys::HtmlElement>().ok()
                                            });
                                            let last = nodes.item(len - 1).and_then(|n| {
                                                n.dyn_into::<web_sys::HtmlElement>().ok()
                                            });
                                            let active = web_sys::window()
                                                .and_then(|w| w.document())
                                                .and_then(|d| d.active_element());
                                            if event.shift_key()
                                                && active.as_ref()
                                                    == first.as_ref().map(|x| x.as_ref())
                                            {
                                                event.prevent_default();
                                                if let Some(last) = last {
                                                    let _ = last.focus();
                                                }
                                            } else if !event.shift_key()
                                                && active.as_ref()
                                                    == last.as_ref().map(|x| x.as_ref())
                                            {
                                                event.prevent_default();
                                                if let Some(first) = first {
                                                    let _ = first.focus();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    })
                })
            } else {
                None
            };
            move || drop(listener)
        });
    }
    {
        let dismiss = props.on_dismiss.clone();
        use_effect_with(props.toast, move |toast| {
            let timer = toast.map(|_| Timeout::new(8_000, move || dismiss.emit(())));
            move || drop(timer)
        });
    }
    let is_ready = |s: &PortalSession| {
        !props.hidden.contains(&s.key)
            && props.rows.get(&s.key).is_some_and(SessionAttention::ready)
    };
    let count = props.sessions.iter().filter(|s| is_ready(s)).count();
    let start = props
        .sessions
        .iter()
        .position(|s| Some(s.key) == props.focused)
        .map_or(0, |i| i + 1);
    let next = props
        .sessions
        .iter()
        .enumerate()
        .cycle()
        .skip(start)
        .take(props.sessions.len())
        .find(|(_, s)| is_ready(s) && Some(s.key) != props.focused)
        .map(|(i, _)| i);
    let selected = props.sessions.iter().find(|s| Some(s.key) == props.focused);
    let open_all = {
        let open = open.clone();
        let ready = ready_only.clone();
        Callback::from(move |_| {
            ready.set(false);
            open.set(true);
        })
    };
    let open_ready = {
        let open = open.clone();
        let ready = ready_only.clone();
        Callback::from(move |_| {
            ready.set(true);
            open.set(true);
        })
    };
    let query = search.to_lowercase();
    let mut rows: Vec<_> = props
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| s.info.scheduled_task_id.is_none())
        .filter(|(_, s)| {
            (!*ready_only || is_ready(s))
                && format!("{} {}", s.info.session_name, s.info.hostname)
                    .to_lowercase()
                    .contains(&query)
        })
        .collect();
    rows.sort_by_key(|(_, s)| {
        (
            !is_ready(s),
            props.hidden.contains(&s.key),
            s.info.session_name.clone(),
        )
    });
    html! {
        <>
            if let Some(session)=props.toast.filter(|k|Some(*k)!=props.focused).and_then(|k|props.sessions.iter().enumerate().find(|(_,s)|s.key==k)) {
                if !props.hidden.contains(&session.1.key) {
                    <div class="ready-toast" role="status">
                        <span>{format!("{} · {}",session.1.info.session_name,props.rows.get(&session.1.key).map_or("New reply",SessionAttention::label))}</span>
                        <button onclick={{let select=props.on_select.clone();let dismiss=props.on_dismiss.clone();let index=session.0; Callback::from(move |_|{select.emit(index);dismiss.emit(());})}}>{"Open"}</button>
                        <button aria-label="Dismiss notification" onclick={props.on_dismiss.reform(|_|())}>{"×"}</button>
                    </div>
                }
            }
            <nav class="mobile-ready-bar" aria-label="Session navigation">
                <button class="ready-current" ref={trigger} onclick={open_all} aria-haspopup="dialog" aria-expanded={open.to_string()}>
                    <span>{selected.map_or("Choose a session",|s|s.info.session_name.as_str())}</span><span aria-hidden="true">{"⌃"}</span>
                </button>
                <button class={classes!("ready-count",(count>0).then_some("has-ready"))} onclick={open_ready} aria-label={format!("Ready sessions: {count}")}>{format!("Ready · {count}")}</button>
                <button class="ready-next" disabled={next.is_none()} aria-label="Next ready session" onclick={{let select=props.on_select.clone();Callback::from(move |_|{if let Some(i)=next{select.emit(i);}})}}>{"Next"}</button>
            </nav>
            if *open {
                <div class="ready-backdrop" onclick={close.reform(|_|())}>
                    <section class="ready-sheet" ref={sheet} role="dialog" aria-modal="true" aria-labelledby="ready-sheet-title" tabindex="-1" onclick={Callback::from(|e:MouseEvent|e.stop_propagation())}>
                        <header><h2 id="ready-sheet-title">{"Sessions"}</h2><button aria-label="Close sessions" onclick={close.reform(|_|())}>{"Done"}</button></header>
                        <div class="ready-filters">
                            <button aria-pressed={(!*ready_only).to_string()} onclick={{let ready=ready_only.clone();Callback::from(move |_|ready.set(false))}}>{"All"}</button>
                            <button aria-pressed={ready_only.to_string()} onclick={{let ready=ready_only.clone();Callback::from(move |_|ready.set(true))}}>{format!("Ready · {count}")}</button>
                            <input aria-label="Find a session" placeholder="Find a session" value={(*search).clone()} oninput={{let search=search.clone();Callback::from(move |e:InputEvent|search.set(e.target_unchecked_into::<web_sys::HtmlInputElement>().value()))}} />
                        </div>
                        if props.unavailable {<p class="ready-status" role="status">{"Reconnecting… statuses may be out of date."}</p>}
                        if props.mutation_failed {<p class="ready-status" role="alert">{"Could not save this session update. Try again."}</p>}
                        <div class="ready-list">
                            if rows.is_empty() {<p class="ready-empty">{if *ready_only {"You're caught up. New responses will appear here."} else {"No matching sessions."}}</p>}
                            {for rows.into_iter().map(|(index,session)|{
                                let status=props.rows.get(&session.key);
                                let label=if session.info.paused {"Paused"} else if props.hidden.contains(&session.key){"Hidden"} else {status.map_or("Checking…",SessionAttention::label)};
                                let muted=status.is_some_and(|s|s.muted);
                                let select=props.on_select.clone();let close=close.clone();
                                html!{<article class={classes!("ready-row",is_ready(session).then_some("needs-attention"))} key={session.key.to_string()}>
                                    <button class="ready-open" onclick={Callback::from(move |_|{select.emit(index);close.emit(());})}>
                                        <span class="ready-row-heading"><strong>{&session.info.session_name}</strong><span class="ready-label">{label}</span></span>
                                        <span class="ready-location">{&session.info.hostname}{if session.portal.is_linked(){format!(" · {}",session.portal.origin())}else{String::new()}}</span>
                                        if let Some(row)=status {
                                            if row.unread && !row.preview.is_empty(){<span class="ready-preview">{&row.preview}</span>}
                                            if let Some(at)=&row.reply_at {<time datetime={at.clone()}>{relative_time(at)}</time>}
                                        }
                                    </button>
                                    <div class="ready-actions">
                                    if let Some(reply_id)=status.filter(|s|s.unread).and_then(|s|s.reply_id) {
                                        <button class="ready-mark-read" disabled={!session.portal.can_write()} aria-label={format!("Mark reply read for {}",session.info.session_name)} onclick={{let mark_read=props.on_mark_read.clone();let key=session.key;Callback::from(move |_|mark_read.emit((key,reply_id)))}}>{"Mark read"}</button>
                                    }
                                    <button class="ready-mute" disabled={!session.portal.can_write()} aria-label={format!("{} notifications for {}",if muted{"Unmute"}else{"Mute"},session.info.session_name)} aria-pressed={muted.to_string()} onclick={{let mute=props.on_mute.clone();let key=session.key;Callback::from(move |_|mute.emit((key,!muted)))}}>{if muted{"Unmute"}else{"Mute"}}</button>
                                    </div>
                                </article>}
                            })}
                        </div>
                        <footer><button onclick={{let close=close.clone();let settings=props.on_settings.clone();Callback::from(move |event|{close.emit(());settings.emit(event);})}}>{"Notification settings"}</button><span>{"Unread replies sync across your devices."}</span></footer>
                    </section>
                </div>
            }
        </>
    }
}
fn relative_time(at: &str) -> String {
    let Ok(time) = chrono::DateTime::parse_from_rfc3339(at) else {
        return String::new();
    };
    let seconds = ((js_sys::Date::now() / 1000.0) as i64 - time.timestamp()).max(0);
    if seconds < 60 {
        "Just now".into()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86400)
    }
}

//! Reuse the installed app's document when a notification opens a session.
use crate::Route;
use gloo::events::EventListener;
use serde::Deserialize;
use uuid::Uuid;
use wasm_bindgen::{JsCast, JsValue};
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Deserialize)]
#[serde(tag = "type")]
enum NotificationMessage {
    #[serde(rename = "portal-notification-client")]
    Identify,
    #[serde(rename = "portal-open-session")]
    Open { session_id: Option<Uuid> },
}

fn handle_message(window: &web_sys::Window, navigator: &Navigator, event: &web_sys::Event) {
    let Some(event) = event.dyn_ref::<web_sys::MessageEvent>() else {
        return;
    };
    let Ok(port) = event.ports().get(0).dyn_into::<web_sys::MessagePort>() else {
        return;
    };
    let Some(message) = js_sys::JSON::stringify(&event.data())
        .ok()
        .and_then(|json| json.as_string())
        .and_then(|json| serde_json::from_str::<NotificationMessage>(&json).ok())
    else {
        return;
    };
    let reply = match message {
        NotificationMessage::Identify => window
            .match_media(concat!(
                "(display-mode: standalone), (display-mode: fullscreen), ",
                "(display-mode: window-controls-overlay)"
            ))
            .ok()
            .flatten()
            .is_some_and(|query| query.matches()),
        NotificationMessage::Open { session_id } => {
            let query: Vec<_> = session_id
                .into_iter()
                .map(|id| ("session", id.to_string()))
                .collect();
            navigator.push_with_query(&Route::Dashboard, &query).is_ok()
        }
    };
    let _ = port.post_message(&JsValue::from_bool(reply));
}

/// Lives inside BrowserRouter, outside the route switch, so Settings and
/// History can receive notification launches too. A reply port acknowledges
/// routing support; old or unloaded clients time out in the worker instead.
#[function_component(NotificationLaunch)]
pub(crate) fn notification_launch() -> Html {
    let navigator = use_navigator();
    use_effect_with(navigator, |navigator| {
        let listener = web_sys::window()
            .zip(navigator.clone())
            .filter(|(window, _)| {
                js_sys::Reflect::has(&window.navigator(), &JsValue::from_str("serviceWorker"))
                    .unwrap_or(false)
            })
            .map(|(window, navigator)| {
                EventListener::new(
                    &window.navigator().service_worker(),
                    "message",
                    move |event| {
                        handle_message(&window, &navigator, event);
                    },
                )
            });
        move || drop(listener)
    });
    html! {}
}

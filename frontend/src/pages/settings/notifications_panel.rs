//! Settings → Notifications panel (mobile-apps plan D3).
//!
//! Two concerns, both graceful about missing capability:
//!   1. **Push on this device** — subscribe/unsubscribe via the browser
//!      `PushManager` (plan D2). Feature-detected: if `Notification` /
//!      `PushManager` are absent, or the server has no VAPID key
//!      (`GET /api/push/vapid-key` 404s), the panel shows an unavailable hint
//!      and never touches the push APIs.
//!   2. **Per-event-kind toggles** — bound to `GET`/`PUT /api/push/prefs` with
//!      the shared [`NotificationPrefs`]. Best-effort: if prefs aren't served
//!      yet the panel falls back to defaults and still lets the user toggle.
//!
//! The subscription's server-side row id is remembered in `localStorage` so a
//! later "disable" can `DELETE` the exact row (the browser only knows its
//! endpoint, not our `Uuid`); missing id just falls back to a browser-only
//! unsubscribe and lets the backend's dead-endpoint prune clean up.

use base64::Engine;
use gloo_net::http::Request;
use shared::api::{
    NotificationPrefs, PushPlatform, PushSubscriptionInfo, RegisterPushSubscriptionRequest,
    VapidKeyResponse,
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::PushEncryptionKeyName;
use yew::prelude::*;

use crate::utils::{self, On401};

/// `localStorage` key holding the server-side subscription id for this device.
const SUB_ID_STORAGE_KEY: &str = "portal_push_subscription_id";

/// High-level availability of push on this device.
#[derive(Clone, PartialEq)]
enum Availability {
    /// Still probing (feature-detect + vapid-key fetch in flight).
    Loading,
    /// Browser lacks `Notification` / `PushManager`, or it's an insecure ctx.
    Unsupported,
    /// APIs present but the server has no VAPID key configured (endpoint 404).
    ServerUnconfigured,
    /// Push is usable; carries the base64url VAPID application-server key.
    Available(String),
}

/// Whether this device currently holds an active push subscription.
#[derive(Clone, Copy, PartialEq)]
enum DeviceState {
    Unknown,
    Subscribed,
    Unsubscribed,
}

#[function_component(NotificationsPanel)]
pub fn notifications_panel() -> Html {
    let availability = use_state(|| Availability::Loading);
    let device_state = use_state(|| DeviceState::Unknown);
    let prefs = use_state(NotificationPrefs::default);
    let busy = use_state(|| false);
    let status = use_state(|| None::<String>);

    // On mount: feature-detect, fetch the VAPID key, read the current
    // subscription, and load prefs.
    {
        let availability = availability.clone();
        let device_state = device_state.clone();
        let prefs = prefs.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if !push_apis_present() {
                    availability.set(Availability::Unsupported);
                    return;
                }
                match fetch_vapid_key().await {
                    Some(key) => availability.set(Availability::Available(key)),
                    None => {
                        availability.set(Availability::ServerUnconfigured);
                        return;
                    }
                }
                device_state.set(if current_subscription_present().await {
                    DeviceState::Subscribed
                } else {
                    DeviceState::Unsubscribed
                });
                if let Some(loaded) = fetch_prefs().await {
                    prefs.set(loaded);
                }
            });
            || ()
        });
    }

    let on_enable = {
        let availability = availability.clone();
        let device_state = device_state.clone();
        let busy = busy.clone();
        let status = status.clone();
        Callback::from(move |_: MouseEvent| {
            let Availability::Available(key) = (*availability).clone() else {
                return;
            };
            let device_state = device_state.clone();
            let busy = busy.clone();
            let status = status.clone();
            busy.set(true);
            status.set(None);
            spawn_local(async move {
                match enable_push(&key).await {
                    Ok(()) => {
                        device_state.set(DeviceState::Subscribed);
                        status.set(Some("Push enabled on this device.".to_string()));
                    }
                    Err(e) => status.set(Some(format!("Could not enable push: {e}"))),
                }
                busy.set(false);
            });
        })
    };

    let on_disable = {
        let device_state = device_state.clone();
        let busy = busy.clone();
        let status = status.clone();
        Callback::from(move |_: MouseEvent| {
            let device_state = device_state.clone();
            let busy = busy.clone();
            let status = status.clone();
            busy.set(true);
            status.set(None);
            spawn_local(async move {
                match disable_push().await {
                    Ok(()) => {
                        device_state.set(DeviceState::Unsubscribed);
                        status.set(Some("Push disabled on this device.".to_string()));
                    }
                    Err(e) => status.set(Some(format!("Could not disable push: {e}"))),
                }
                busy.set(false);
            });
        })
    };

    // One toggle handler factory for the per-event-kind checkboxes: mutate the
    // matching field, persist via PUT, keep local state in sync.
    let make_pref_toggle = {
        let prefs = prefs.clone();
        move |field: PrefField| {
            let prefs = prefs.clone();
            Callback::from(move |e: Event| {
                let checked = e
                    .target_dyn_into::<web_sys::HtmlInputElement>()
                    .map(|i| i.checked())
                    .unwrap_or(false);
                let mut next = *prefs;
                field.set(&mut next, checked);
                prefs.set(next);
                spawn_local(async move {
                    save_prefs(&next).await;
                });
            })
        }
    };

    html! {
        <section class="notifications-section">
            <div class="section-header">
                <h2>{ "Push Notifications" }</h2>
                <p class="section-description">
                    { "Get notified when an agent needs you — permission requests, \
                       turn completion, disconnects, and inter-agent messages." }
                </p>
            </div>

            { render_device_control(
                &availability,
                *device_state,
                *busy,
                &status,
                &on_enable,
                &on_disable,
            ) }

            <div class="notification-prefs">
                <h3>{ "Notify me about" }</h3>
                <p class="section-description">
                    { "Applies to every device you've enabled push on." }
                </p>
                { for PREF_FIELDS.iter().map(|(field, label)| {
                    let toggle = make_pref_toggle(*field);
                    html! {
                        <label class="toggle-label" key={*label}>
                            <span>{ *label }</span>
                            <input
                                type="checkbox"
                                checked={field.get(&prefs)}
                                onchange={toggle}
                            />
                        </label>
                    }
                }) }
            </div>
        </section>
    }
}

/// Render the "push on this device" control, mirroring the voice-input
/// unsupported-hint pattern for the disabled states.
fn render_device_control(
    availability: &Availability,
    device_state: DeviceState,
    busy: bool,
    status: &Option<String>,
    on_enable: &Callback<MouseEvent>,
    on_disable: &Callback<MouseEvent>,
) -> Html {
    let hint = |msg: &str| -> Html {
        html! {
            <div class="push-device-control unavailable">
                <p class="unsupported-hint">{ msg }</p>
            </div>
        }
    };

    match availability {
        Availability::Loading => html! {
            <div class="push-device-control">
                <p class="section-description">{ "Checking push availability…" }</p>
            </div>
        },
        Availability::Unsupported => hint(
            "Push notifications aren't supported in this browser (or this page \
             isn't served over HTTPS).",
        ),
        Availability::ServerUnconfigured => {
            hint("Push notifications aren't configured on this server yet.")
        }
        Availability::Available(_) => {
            let subscribed = device_state == DeviceState::Subscribed;
            html! {
                <div class="push-device-control">
                    <label class="toggle-label">
                        <span>{ "Push on this device" }</span>
                        if subscribed {
                            <button
                                class="create-button"
                                onclick={on_disable.clone()}
                                disabled={busy}
                            >
                                { if busy { "Working…" } else { "Disable" } }
                            </button>
                        } else {
                            <button
                                class="create-button"
                                onclick={on_enable.clone()}
                                disabled={busy}
                            >
                                { if busy { "Working…" } else { "Enable" } }
                            </button>
                        }
                    </label>
                    if let Some(msg) = status {
                        <p class="section-description">{ msg.clone() }</p>
                    }
                </div>
            }
        }
    }
}

// --- Per-event-kind pref plumbing ------------------------------------------

/// Which [`NotificationPrefs`] field a toggle drives. Keeps the checkbox list
/// data-driven so a new event kind is one array entry, not a new branch.
#[derive(Clone, Copy, PartialEq)]
enum PrefField {
    PermissionRequest,
    TurnComplete,
    SessionDisconnected,
    AgentMessage,
}

impl PrefField {
    fn get(&self, p: &NotificationPrefs) -> bool {
        match self {
            PrefField::PermissionRequest => p.permission_request,
            PrefField::TurnComplete => p.turn_complete,
            PrefField::SessionDisconnected => p.session_disconnected,
            PrefField::AgentMessage => p.agent_message,
        }
    }
    fn set(&self, p: &mut NotificationPrefs, v: bool) {
        match self {
            PrefField::PermissionRequest => p.permission_request = v,
            PrefField::TurnComplete => p.turn_complete = v,
            PrefField::SessionDisconnected => p.session_disconnected = v,
            PrefField::AgentMessage => p.agent_message = v,
        }
    }
}

const PREF_FIELDS: [(PrefField, &str); 4] = [
    (PrefField::PermissionRequest, "Permission requests"),
    (PrefField::TurnComplete, "Turn complete"),
    (PrefField::SessionDisconnected, "Session disconnected"),
    (PrefField::AgentMessage, "Agent messages"),
];

// --- Prefs HTTP ------------------------------------------------------------

async fn fetch_prefs() -> Option<NotificationPrefs> {
    utils::fetch_json::<NotificationPrefs>("/api/push/prefs", On401::Ignore)
        .await
        .ok()
}

/// Persist prefs; best-effort (the endpoint may not be deployed yet).
async fn save_prefs(prefs: &NotificationPrefs) {
    let url = utils::api_url("/api/push/prefs");
    if let Ok(req) = Request::put(&url).json(prefs) {
        let _ = req.send().await;
    }
}

// --- Push subscribe flow (web-sys PushManager) -----------------------------

/// Feature-detect: both `Notification` and a real `PushManager` must exist.
fn push_apis_present() -> bool {
    let Some(win) = web_sys::window() else {
        return false;
    };
    let has = |name: &str| js_sys::Reflect::has(&win, &JsValue::from_str(name)).unwrap_or(false);
    has("Notification") && has("PushManager")
}

/// Fetch the server VAPID key; `None` when the endpoint 404s (push disabled).
async fn fetch_vapid_key() -> Option<String> {
    utils::fetch_json::<VapidKeyResponse>("/api/push/vapid-key", On401::Ignore)
        .await
        .ok()
        .map(|r| r.public_key)
}

/// Resolve the active service worker's `PushManager`.
async fn push_manager() -> Result<web_sys::PushManager, JsValue> {
    let win = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let ready = win.navigator().service_worker().ready()?;
    let reg = JsFuture::from(ready).await?;
    let reg: web_sys::ServiceWorkerRegistration = reg.dyn_into()?;
    reg.push_manager()
}

/// Is there already a subscription for this device?
async fn current_subscription_present() -> bool {
    let Ok(pm) = push_manager().await else {
        return false;
    };
    let Ok(promise) = pm.get_subscription() else {
        return false;
    };
    match JsFuture::from(promise).await {
        Ok(v) => !v.is_null() && !v.is_undefined(),
        Err(_) => false,
    }
}

/// Request permission, subscribe, and register the subscription server-side.
async fn enable_push(vapid_b64url: &str) -> Result<(), String> {
    let permission = JsFuture::from(web_sys::Notification::request_permission().map_err(js_err)?)
        .await
        .map_err(js_err)?;
    if permission.as_string().as_deref() != Some("granted") {
        return Err("notification permission was not granted".to_string());
    }

    let pm = push_manager().await.map_err(js_err)?;
    let key_bytes = decode_vapid(vapid_b64url)?;
    let key_arr = js_sys::Uint8Array::from(key_bytes.as_slice());

    // Build the options dict via Reflect so we don't depend on the exact
    // (version-churny) web-sys dictionary setter names.
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &JsValue::from_str("userVisibleOnly"), &JsValue::TRUE)
        .map_err(js_err)?;
    js_sys::Reflect::set(&opts, &JsValue::from_str("applicationServerKey"), &key_arr)
        .map_err(js_err)?;
    let opts: web_sys::PushSubscriptionOptionsInit = opts.unchecked_into();

    let sub = JsFuture::from(pm.subscribe_with_options(&opts).map_err(js_err)?)
        .await
        .map_err(js_err)?;
    let sub: web_sys::PushSubscription = sub.dyn_into().map_err(js_err)?;

    let request = sub_to_request(&sub)?;
    register_subscription(&request).await
}

/// Unsubscribe in the browser and delete the server-side row if we know its id.
async fn disable_push() -> Result<(), String> {
    let pm = push_manager().await.map_err(js_err)?;
    let existing = JsFuture::from(pm.get_subscription().map_err(js_err)?)
        .await
        .map_err(js_err)?;
    if !existing.is_null() && !existing.is_undefined() {
        if let Ok(sub) = existing.dyn_into::<web_sys::PushSubscription>() {
            let _ = JsFuture::from(sub.unsubscribe().map_err(js_err)?)
                .await
                .map_err(js_err)?;
        }
    }
    // Delete the server row we recorded at subscribe time, if any.
    if let Some(id) = utils::storage_get(SUB_ID_STORAGE_KEY) {
        let url = utils::api_url(&format!("/api/push/subscriptions/{id}"));
        let _ = Request::delete(&url).send().await;
        utils::storage_remove(SUB_ID_STORAGE_KEY);
    }
    Ok(())
}

/// Turn a browser [`web_sys::PushSubscription`] into the register payload,
/// base64url-encoding the p256dh/auth keys the backend needs to encrypt pushes.
fn sub_to_request(
    sub: &web_sys::PushSubscription,
) -> Result<RegisterPushSubscriptionRequest, String> {
    Ok(RegisterPushSubscriptionRequest {
        platform: PushPlatform::Webpush,
        endpoint_or_token: sub.endpoint(),
        p256dh: Some(key_b64url(sub, PushEncryptionKeyName::P256dh)?),
        auth: Some(key_b64url(sub, PushEncryptionKeyName::Auth)?),
        device_label: web_sys::window().map(|w| w.navigator().user_agent().unwrap_or_default()),
    })
}

/// base64url-encode one subscription key (`p256dh` or `auth`).
fn key_b64url(
    sub: &web_sys::PushSubscription,
    name: PushEncryptionKeyName,
) -> Result<String, String> {
    let buf = sub
        .get_key(name)
        .map_err(js_err)?
        .ok_or_else(|| "subscription key missing".to_string())?;
    let bytes = js_sys::Uint8Array::new(&buf).to_vec();
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// POST the subscription and remember its server-side id for later deletion.
async fn register_subscription(req: &RegisterPushSubscriptionRequest) -> Result<(), String> {
    let url = utils::api_url("/api/push/subscriptions");
    let request = Request::post(&url).json(req).map_err(|e| e.to_string())?;
    let resp = request.send().await.map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(format!("server returned {}", resp.status()));
    }
    if let Ok(info) = resp.json::<PushSubscriptionInfo>().await {
        utils::storage_set(SUB_ID_STORAGE_KEY, &info.id.to_string());
    }
    Ok(())
}

/// Decode a base64url (optionally padded, optionally standard-alphabet) VAPID
/// key into raw bytes for `applicationServerKey`.
fn decode_vapid(key: &str) -> Result<Vec<u8>, String> {
    let normalized: String = key
        .trim()
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            other => other,
        })
        .collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(normalized.as_bytes())
        .map_err(|e| format!("invalid VAPID key: {e}"))
}

/// Render a `JsValue` error as a short string for the status line.
fn js_err(v: JsValue) -> String {
    if let Some(s) = v.as_string() {
        return s;
    }
    if let Some(msg) = v.dyn_ref::<js_sys::Error>().map(|e| e.message()) {
        return msg.into();
    }
    format!("{v:?}")
}

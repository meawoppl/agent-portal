//! Voice Input Component
//!
//! Browser-native voice input using the Web Speech API (`SpeechRecognition` /
//! `webkitSpeechRecognition`). Recognition runs entirely in the user's browser;
//! the backend is not involved.
//!
//! When the API is not available (e.g. Firefox), the button is rendered in a
//! greyed-out unsupported state. Hovering shows a native tooltip and clicking
//! pops a short hint explaining the browser limitation.
//!
//! iOS-specific behavior (see #840): iOS WebKit allows only one active
//! `SpeechRecognition` per page, and races the permission prompt against
//! `recognition.start()`. We work around this by (1) priming mic permission
//! via `getUserMedia({audio:true})` and holding the stream open for the
//! recognizer's lifetime, and (2) tracking a `pending_stop` flag so a new
//! session can't start until the previous one's `onend` has fired.

use gloo::timers::callback::Timeout;
use js_sys::{Array, Function, Object, Promise, Reflect};
use uuid::Uuid;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use yew::prelude::*;

const UNSUPPORTED_HINT: &str = "Voice input needs the Web Speech API. Try Chrome, Edge, or Safari.";
const BUSY_HINT: &str = "Voice recognizer is busy — wait a moment and tap again.";
const MIC_DENIED_HINT: &str =
    "Microphone permission was denied. Enable it in your browser settings and try again.";

/// Return the `SpeechRecognition` (or `webkitSpeechRecognition`) constructor if
/// available on the current `window`.
fn speech_recognition_ctor() -> Option<Function> {
    let window = web_sys::window()?;
    for name in ["SpeechRecognition", "webkitSpeechRecognition"] {
        if let Ok(v) = Reflect::get(&window, &JsValue::from_str(name)) {
            if !v.is_undefined() && !v.is_null() {
                if let Ok(func) = v.dyn_into::<Function>() {
                    return Some(func);
                }
            }
        }
    }
    None
}

fn is_speech_recognition_supported() -> bool {
    speech_recognition_ctor().is_some()
}

/// Prime mic permission via `navigator.mediaDevices.getUserMedia({audio:true})`.
/// Returns the `MediaStream` on success so the caller can hold it open for the
/// recognizer's lifetime — that keeps iOS from racing the permission prompt
/// against `recognition.start()` (#840).
async fn request_mic_stream() -> Result<JsValue, String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let navigator = window.navigator();
    let media_devices = Reflect::get(&navigator, &JsValue::from_str("mediaDevices"))
        .map_err(|_| "navigator.mediaDevices missing".to_string())?;
    if media_devices.is_undefined() || media_devices.is_null() {
        return Err("navigator.mediaDevices missing".to_string());
    }

    let constraints = Object::new();
    let _ = Reflect::set(&constraints, &JsValue::from_str("audio"), &JsValue::TRUE);

    let get_user_media = Reflect::get(&media_devices, &JsValue::from_str("getUserMedia"))
        .map_err(|_| "getUserMedia missing".to_string())?
        .dyn_into::<Function>()
        .map_err(|_| "getUserMedia not callable".to_string())?;

    let promise_val = get_user_media
        .call1(&media_devices, &constraints)
        .map_err(|e| format!("getUserMedia call failed: {:?}", e))?;
    let promise = promise_val
        .dyn_into::<Promise>()
        .map_err(|_| "getUserMedia did not return a Promise".to_string())?;

    JsFuture::from(promise).await.map_err(|e| {
        // iOS surfaces DOMException with .name like "NotAllowedError"
        let name = Reflect::get(&e, &JsValue::from_str("name"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| format!("{:?}", e));
        name
    })
}

/// Stop every track on a `MediaStream` we held open via `getUserMedia`.
fn stop_media_stream(stream: &JsValue) {
    let Ok(get_tracks) = Reflect::get(stream, &JsValue::from_str("getTracks")) else {
        return;
    };
    let Ok(get_tracks_fn) = get_tracks.dyn_into::<Function>() else {
        return;
    };
    let Ok(tracks_val) = get_tracks_fn.call0(stream) else {
        return;
    };
    let length = Reflect::get(&tracks_val, &JsValue::from_str("length"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;
    for i in 0..length {
        let Ok(track) = Reflect::get(&tracks_val, &JsValue::from_f64(i as f64)) else {
            continue;
        };
        if let Ok(stop_fn) =
            Reflect::get(&track, &JsValue::from_str("stop")).and_then(|f| f.dyn_into::<Function>())
        {
            let _ = stop_fn.call0(&track);
        }
    }
}

/// Props for the VoiceInput component
#[derive(Properties, PartialEq)]
pub struct VoiceInputProps {
    /// Session ID — retained so callers can keep using `{session_id}` without
    /// caring that the component no longer needs it.
    #[prop_or_default]
    pub session_id: Option<Uuid>,
    /// Callback when recording state changes
    pub on_recording_change: Callback<bool>,
    /// Callback when a final transcription is ready to send
    pub on_transcription: Callback<String>,
    /// Callback for interim (partial) transcription updates
    #[prop_or_default]
    pub on_interim_transcription: Option<Callback<String>>,
    /// Callback when an error occurs
    pub on_error: Callback<String>,
    /// Whether the component is disabled (e.g. WebSocket not connected)
    #[prop_or(false)]
    pub disabled: bool,
    /// Optional NodeRef to attach to the button for programmatic control
    #[prop_or_default]
    pub button_ref: Option<NodeRef>,
}

pub enum VoiceInputMsg {
    ToggleRecording,
    SessionStarted(ActiveSession),
    StartFailed(StartFailure),
    Final(String),
    Interim(String),
    /// `(kind, message)` — `event.error` and the full `event.message` text.
    /// `message` is needed to discriminate iOS's `aborted/"Another request is
    /// started"` from a plain user-initiated abort.
    RecognitionError(String, String),
    Ended,
    HideHint,
}

/// Reasons the async start path can bail before a session is established.
pub enum StartFailure {
    /// The user denied mic permission (or the browser blocked it).
    PermissionDenied,
    /// Anything else — surfaced to the parent via `on_error`.
    Other(String),
}

/// Owns the active `SpeechRecognition` instance, the primer `MediaStream`,
/// and the closures so they live as long as the session does and are cleaned
/// up on drop.
pub struct ActiveSession {
    recognition: JsValue,
    /// Held open for the recognizer's lifetime. Stopping it on Drop releases
    /// the mic indicator; iOS treats the live stream as proof of an
    /// uninterrupted permission grant, which avoids re-prompting mid-session.
    mic_stream: JsValue,
    _on_result: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
    _on_end: Closure<dyn FnMut(JsValue)>,
}

impl Drop for ActiveSession {
    fn drop(&mut self) {
        if let Ok(stop) = Reflect::get(&self.recognition, &JsValue::from_str("stop")) {
            if let Ok(stop_fn) = stop.dyn_into::<Function>() {
                let _ = stop_fn.call0(&self.recognition);
            }
        }
        // Detach handlers so any late-firing event from the browser is a no-op.
        // The `onend` from this stop() will fire after Drop returns; the
        // component's `pending_stop` flag is what unblocks the next start.
        for prop in ["onresult", "onerror", "onend"] {
            let _ = Reflect::set(&self.recognition, &JsValue::from_str(prop), &JsValue::NULL);
        }
        stop_media_stream(&self.mic_stream);
    }
}

pub struct VoiceInput {
    supported: bool,
    /// `true` once the user has asked to record (optimistic — set before the
    /// async permission primer resolves) and stays `true` until the session
    /// ends via stop, error, or natural `onend`.
    is_recording: bool,
    /// `true` while a start request is in flight (`getUserMedia` await + SR
    /// construction). New `ToggleRecording` taps during this window are
    /// ignored. Cleared by `SessionStarted` or `StartFailed`.
    is_starting: bool,
    /// `true` from the moment we drop a session to when its `onend` arrives.
    /// New starts are refused with the busy hint until this clears, because
    /// iOS WebKit will reject a second `start()` with
    /// `aborted/"Another request is started"`.
    pending_stop: bool,
    session: Option<ActiveSession>,
    hint_message: Option<&'static str>,
    hint_timer: Option<Timeout>,
}

impl Component for VoiceInput {
    type Message = VoiceInputMsg;
    type Properties = VoiceInputProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            supported: is_speech_recognition_supported(),
            is_recording: false,
            is_starting: false,
            pending_stop: false,
            session: None,
            hint_message: None,
            hint_timer: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            VoiceInputMsg::ToggleRecording => {
                if !self.supported {
                    self.show_hint(ctx, UNSUPPORTED_HINT);
                    return true;
                }
                if self.pending_stop || self.is_starting {
                    self.show_hint(ctx, BUSY_HINT);
                    return true;
                }

                if self.is_recording {
                    // User asked to stop. Drop the session; its onend will
                    // fire VoiceInputMsg::Ended which clears pending_stop.
                    self.session = None;
                    self.is_recording = false;
                    self.pending_stop = true;
                    ctx.props().on_recording_change.emit(false);
                    return true;
                }

                // Optimistic UI: light up the recording icon immediately so
                // the user has feedback while we wait for getUserMedia + start.
                self.is_recording = true;
                self.is_starting = true;
                ctx.props().on_recording_change.emit(true);

                let link = ctx.link().clone();
                spawn_local(async move {
                    match start_session_async(link.clone()).await {
                        Ok(session) => {
                            link.send_message(VoiceInputMsg::SessionStarted(session));
                        }
                        Err(failure) => {
                            link.send_message(VoiceInputMsg::StartFailed(failure));
                        }
                    }
                });
                true
            }
            VoiceInputMsg::SessionStarted(session) => {
                self.is_starting = false;
                if !self.is_recording {
                    // The user toggled off (or hit an error) while we were
                    // waiting on the permission primer. Drop the session we
                    // just built so we don't leak a stray recognizer.
                    drop(session);
                    return true;
                }
                self.session = Some(session);
                true
            }
            VoiceInputMsg::StartFailed(failure) => {
                self.is_starting = false;
                self.is_recording = false;
                ctx.props().on_recording_change.emit(false);
                match failure {
                    StartFailure::PermissionDenied => {
                        self.show_hint(ctx, MIC_DENIED_HINT);
                    }
                    StartFailure::Other(message) => {
                        ctx.props().on_error.emit(message);
                    }
                }
                true
            }
            VoiceInputMsg::Final(text) => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    ctx.props().on_transcription.emit(trimmed.to_string());
                }
                false
            }
            VoiceInputMsg::Interim(text) => {
                if let Some(cb) = ctx.props().on_interim_transcription.as_ref() {
                    cb.emit(text);
                }
                false
            }
            VoiceInputMsg::RecognitionError(kind, message) => {
                let is_ios_singleton_conflict =
                    kind == "aborted" && message.to_ascii_lowercase().contains("another request");
                let is_permission = kind == "not-allowed" || kind == "service-not-allowed";
                let is_silent_benign = kind == "no-speech" || kind == "aborted";

                if is_ios_singleton_conflict {
                    self.show_hint(ctx, BUSY_HINT);
                } else if is_permission {
                    self.show_hint(ctx, MIC_DENIED_HINT);
                } else if !is_silent_benign {
                    ctx.props().on_error.emit(kind);
                }

                // Any error tears down the session — fall through to the
                // same cleanup as Ended.
                self.session = None;
                if self.is_recording {
                    self.is_recording = false;
                    self.pending_stop = true;
                    ctx.props().on_recording_change.emit(false);
                }
                true
            }
            VoiceInputMsg::Ended => {
                self.session = None;
                self.pending_stop = false;
                if self.is_recording {
                    self.is_recording = false;
                    ctx.props().on_recording_change.emit(false);
                }
                true
            }
            VoiceInputMsg::HideHint => {
                self.hint_timer = None;
                if self.hint_message.is_some() {
                    self.hint_message = None;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let onclick = ctx.link().callback(|e: MouseEvent| {
            e.prevent_default();
            VoiceInputMsg::ToggleRecording
        });

        let hard_disabled = ctx.props().disabled;
        let button_class = classes!(
            "voice-button",
            self.is_recording.then_some("recording"),
            (!self.supported).then_some("unsupported"),
        );

        let title = if !self.supported {
            UNSUPPORTED_HINT
        } else if self.is_recording {
            "Stop recording (Ctrl+M)"
        } else {
            "Start voice input (Ctrl+M)"
        };

        let button_ref = ctx.props().button_ref.clone().unwrap_or_default();

        html! {
            <div class="voice-button-wrapper">
                <button
                    ref={button_ref}
                    class={button_class}
                    onclick={onclick}
                    disabled={hard_disabled}
                    aria-disabled={(!self.supported).then_some("true")}
                    title={title}
                    type="button"
                >
                    if self.is_recording {
                        <span class="voice-icon recording-icon">{ "\u{1F534}" }</span>
                    } else if !self.supported {
                        <span class="voice-icon mic-icon unsupported">{ "\u{1F507}" }</span>
                    } else {
                        <span class="voice-icon mic-icon">{ "\u{1F3A4}" }</span>
                    }
                </button>
                if let Some(hint) = self.hint_message {
                    <div class="voice-tooltip" role="tooltip">{ hint }</div>
                }
            </div>
        }
    }

    fn destroy(&mut self, _ctx: &Context<Self>) {
        self.session = None;
        self.hint_timer = None;
    }
}

impl VoiceInput {
    fn show_hint(&mut self, ctx: &Context<Self>, message: &'static str) {
        self.hint_message = Some(message);
        let link = ctx.link().clone();
        self.hint_timer = Some(Timeout::new(4000, move || {
            link.send_message(VoiceInputMsg::HideHint);
        }));
    }
}

/// Async start path: prime mic permission via `getUserMedia`, then construct
/// and `.start()` the recognizer. Holding the primer `MediaStream` open until
/// the session is dropped keeps iOS from re-racing the permission prompt
/// against the recognizer.
async fn start_session_async(
    link: yew::html::Scope<VoiceInput>,
) -> Result<ActiveSession, StartFailure> {
    // 1. Permission primer. iOS shows its prompt here, *before* SpeechRecognition
    //    is even constructed — eliminates the prompt-vs-start race.
    let mic_stream = request_mic_stream().await.map_err(|name| {
        log::warn!("getUserMedia rejected: {}", name);
        if name == "NotAllowedError" || name == "PermissionDeniedError" {
            StartFailure::PermissionDenied
        } else {
            StartFailure::Other(format!("Could not access microphone: {}", name))
        }
    })?;

    // 2. Build the SpeechRecognition with permission already granted.
    let ctor = speech_recognition_ctor()
        .ok_or_else(|| StartFailure::Other("SpeechRecognition not available".into()))?;
    let recognition = Reflect::construct(&ctor, &Array::new()).map_err(|_| {
        // Release the primer stream before bailing.
        stop_media_stream(&mic_stream);
        StartFailure::Other("Failed to construct SpeechRecognition".into())
    })?;

    let set_bool = |name: &str, val: bool| {
        let _ = Reflect::set(
            &recognition,
            &JsValue::from_str(name),
            &JsValue::from_bool(val),
        );
    };
    set_bool("continuous", true);
    set_bool("interimResults", true);

    let lang = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
        .and_then(|el| el.get_attribute("lang"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "en-US".to_string());
    let _ = Reflect::set(
        &recognition,
        &JsValue::from_str("lang"),
        &JsValue::from_str(&lang),
    );

    let final_acc: std::rc::Rc<std::cell::RefCell<String>> = Default::default();

    let link_for_result = link.clone();
    let final_for_result = final_acc.clone();
    let on_result = Closure::wrap(Box::new(move |event: JsValue| {
        let results = match Reflect::get(&event, &JsValue::from_str("results")) {
            Ok(v) => v,
            Err(_) => return,
        };
        let result_index = Reflect::get(&event, &JsValue::from_str("resultIndex"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as u32;
        let length = Reflect::get(&results, &JsValue::from_str("length"))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as u32;

        let mut interim = String::new();
        for i in result_index..length {
            let Ok(result) = Reflect::get(&results, &JsValue::from_f64(i as f64)) else {
                continue;
            };
            let is_final = Reflect::get(&result, &JsValue::from_str("isFinal"))
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let alt = match Reflect::get(&result, &JsValue::from_f64(0.0)) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let transcript = Reflect::get(&alt, &JsValue::from_str("transcript"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            if is_final {
                let mut acc = final_for_result.borrow_mut();
                if !acc.is_empty() && !acc.ends_with(' ') {
                    acc.push(' ');
                }
                acc.push_str(transcript.trim());
            } else if !transcript.is_empty() {
                if !interim.is_empty() {
                    interim.push(' ');
                }
                interim.push_str(&transcript);
            }
        }

        if !interim.is_empty() {
            link_for_result.send_message(VoiceInputMsg::Interim(interim));
        } else {
            link_for_result.send_message(VoiceInputMsg::Interim(String::new()));
        }
    }) as Box<dyn FnMut(JsValue)>);

    let link_for_error = link.clone();
    let on_error = Closure::wrap(Box::new(move |event: JsValue| {
        let kind = Reflect::get(&event, &JsValue::from_str("error"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "unknown".to_string());
        let message = Reflect::get(&event, &JsValue::from_str("message"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        log::warn!("SpeechRecognition error: {} ({})", kind, message);
        link_for_error.send_message(VoiceInputMsg::RecognitionError(kind, message));
    }) as Box<dyn FnMut(JsValue)>);

    let link_for_end = link.clone();
    let final_for_end = final_acc.clone();
    let on_end = Closure::wrap(Box::new(move |_event: JsValue| {
        let text = std::mem::take(&mut *final_for_end.borrow_mut());
        if !text.trim().is_empty() {
            link_for_end.send_message(VoiceInputMsg::Final(text));
        }
        link_for_end.send_message(VoiceInputMsg::Ended);
    }) as Box<dyn FnMut(JsValue)>);

    let set_handler = |name: &str, closure: &Closure<dyn FnMut(JsValue)>| {
        Reflect::set(
            &recognition,
            &JsValue::from_str(name),
            closure.as_ref().unchecked_ref(),
        )
    };
    if set_handler("onresult", &on_result).is_err()
        || set_handler("onerror", &on_error).is_err()
        || set_handler("onend", &on_end).is_err()
    {
        stop_media_stream(&mic_stream);
        return Err(StartFailure::Other(
            "Failed to attach recognizer handlers".into(),
        ));
    }

    let start_fn = Reflect::get(&recognition, &JsValue::from_str("start"))
        .ok()
        .and_then(|v| v.dyn_into::<Function>().ok())
        .ok_or_else(|| {
            stop_media_stream(&mic_stream);
            StartFailure::Other("SpeechRecognition.start is not callable".into())
        })?;

    if let Err(e) = start_fn.call0(&recognition) {
        stop_media_stream(&mic_stream);
        return Err(StartFailure::Other(format!(
            "Could not start microphone: {:?}",
            e
        )));
    }

    Ok(ActiveSession {
        recognition,
        mic_stream,
        _on_result: on_result,
        _on_error: on_error,
        _on_end: on_end,
    })
}

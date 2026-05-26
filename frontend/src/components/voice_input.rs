//! Voice Input Component
//!
//! Browser-native voice input using the Web Speech API (`SpeechRecognition` /
//! `webkitSpeechRecognition`). Recognition runs entirely in the user's browser;
//! the backend is not involved.
//!
//! When the API is not available (e.g. Firefox), the button is rendered in a
//! greyed-out unsupported state. Hovering shows a native tooltip and clicking
//! pops a short hint explaining the browser limitation.

use gloo::timers::callback::Timeout;
use js_sys::{Array, Function, Reflect};
use uuid::Uuid;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use yew::prelude::*;

const UNSUPPORTED_HINT: &str =
    "Voice input needs the Web Speech API. Try Chrome, Edge, or Safari.";

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
    Final(String),
    Interim(String),
    Error(String),
    Ended,
    HideUnsupportedHint,
}

/// Owns the active `SpeechRecognition` instance and its closures so they live
/// as long as the session does and are cleaned up on drop.
struct ActiveSession {
    recognition: JsValue,
    /// We track final text in a shared cell so the `onresult` closure can append.
    /// On stop we drain it and emit.
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
        for prop in ["onresult", "onerror", "onend"] {
            let _ = Reflect::set(&self.recognition, &JsValue::from_str(prop), &JsValue::NULL);
        }
    }
}

pub struct VoiceInput {
    supported: bool,
    is_recording: bool,
    session: Option<ActiveSession>,
    show_unsupported_hint: bool,
    hint_timer: Option<Timeout>,
}

impl Component for VoiceInput {
    type Message = VoiceInputMsg;
    type Properties = VoiceInputProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            supported: is_speech_recognition_supported(),
            is_recording: false,
            session: None,
            show_unsupported_hint: false,
            hint_timer: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            VoiceInputMsg::ToggleRecording => {
                if !self.supported {
                    // Show the click-tooltip; auto-hide after a few seconds.
                    self.show_unsupported_hint = true;
                    let link = ctx.link().clone();
                    self.hint_timer = Some(Timeout::new(4000, move || {
                        link.send_message(VoiceInputMsg::HideUnsupportedHint);
                    }));
                    return true;
                }

                if self.is_recording {
                    // Drop the session — Drop impl calls .stop() which will
                    // trigger `onend`, which clears state via VoiceInputMsg::Ended.
                    self.session = None;
                    self.is_recording = false;
                    ctx.props().on_recording_change.emit(false);
                    return true;
                }

                match self.start_session(ctx) {
                    Ok(session) => {
                        self.session = Some(session);
                        self.is_recording = true;
                        ctx.props().on_recording_change.emit(true);
                        true
                    }
                    Err(e) => {
                        ctx.props().on_error.emit(e);
                        false
                    }
                }
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
            VoiceInputMsg::Error(message) => {
                // "no-speech" and "aborted" are routine — surface only as a soft signal.
                let benign = matches!(message.as_str(), "no-speech" | "aborted");
                if !benign {
                    ctx.props().on_error.emit(message);
                }
                self.session = None;
                if self.is_recording {
                    self.is_recording = false;
                    ctx.props().on_recording_change.emit(false);
                }
                true
            }
            VoiceInputMsg::Ended => {
                self.session = None;
                if self.is_recording {
                    self.is_recording = false;
                    ctx.props().on_recording_change.emit(false);
                    true
                } else {
                    false
                }
            }
            VoiceInputMsg::HideUnsupportedHint => {
                self.hint_timer = None;
                if self.show_unsupported_hint {
                    self.show_unsupported_hint = false;
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
                if self.show_unsupported_hint {
                    <div class="voice-tooltip" role="tooltip">
                        { UNSUPPORTED_HINT }
                    </div>
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
    fn start_session(&self, ctx: &Context<Self>) -> Result<ActiveSession, String> {
        let ctor = speech_recognition_ctor()
            .ok_or_else(|| "SpeechRecognition not available in this browser".to_string())?;
        let recognition = Reflect::construct(&ctor, &Array::new())
            .map_err(|_| "Failed to construct SpeechRecognition".to_string())?;

        let set_bool = |name: &str, val: bool| {
            let _ = Reflect::set(&recognition, &JsValue::from_str(name), &JsValue::from_bool(val));
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

        // Accumulated final transcript for this session. Each fresh final segment
        // is appended; on `onend` we emit the trimmed result.
        let final_acc: std::rc::Rc<std::cell::RefCell<String>> = Default::default();

        let link = ctx.link().clone();
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
                link.send_message(VoiceInputMsg::Interim(interim));
            } else {
                // Clear any stale interim text once a final lands.
                link.send_message(VoiceInputMsg::Interim(String::new()));
            }
        }) as Box<dyn FnMut(JsValue)>);

        let link = ctx.link().clone();
        let on_error = Closure::wrap(Box::new(move |event: JsValue| {
            let kind = Reflect::get(&event, &JsValue::from_str("error"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "unknown".to_string());
            log::warn!("SpeechRecognition error: {}", kind);
            link.send_message(VoiceInputMsg::Error(kind));
        }) as Box<dyn FnMut(JsValue)>);

        let link = ctx.link().clone();
        let final_for_end = final_acc.clone();
        let on_end = Closure::wrap(Box::new(move |_event: JsValue| {
            let text = std::mem::take(&mut *final_for_end.borrow_mut());
            if !text.trim().is_empty() {
                link.send_message(VoiceInputMsg::Final(text));
            }
            link.send_message(VoiceInputMsg::Ended);
        }) as Box<dyn FnMut(JsValue)>);

        Reflect::set(
            &recognition,
            &JsValue::from_str("onresult"),
            on_result.as_ref().unchecked_ref(),
        )
        .map_err(|_| "Failed to set onresult".to_string())?;
        Reflect::set(
            &recognition,
            &JsValue::from_str("onerror"),
            on_error.as_ref().unchecked_ref(),
        )
        .map_err(|_| "Failed to set onerror".to_string())?;
        Reflect::set(
            &recognition,
            &JsValue::from_str("onend"),
            on_end.as_ref().unchecked_ref(),
        )
        .map_err(|_| "Failed to set onend".to_string())?;

        let start_fn = Reflect::get(&recognition, &JsValue::from_str("start"))
            .map_err(|_| "SpeechRecognition has no start()".to_string())?
            .dyn_into::<Function>()
            .map_err(|_| "SpeechRecognition.start is not callable".to_string())?;
        start_fn
            .call0(&recognition)
            .map_err(|e| format!("Could not start microphone: {:?}", e))?;

        Ok(ActiveSession {
            recognition,
            _on_result: on_result,
            _on_error: on_error,
            _on_end: on_end,
        })
    }
}

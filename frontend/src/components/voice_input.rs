//! Voice Input Component
//!
//! Two capture strategies behind one button, chosen by `server_stt` (which the
//! dashboard sets from `AppConfig::stt_enabled`):
//!
//! * **Server** — record the utterance with `MediaRecorder` and POST it to
//!   `/api/stt/transcribe`. Works in every browser we target, and the server
//!   biases the recognizer with the session's vocabulary, which is the whole
//!   reason to prefer it.
//! * **Browser** — the Web Speech API (`SpeechRecognition` /
//!   `webkitSpeechRecognition`), used when no provider is configured. Needs no
//!   credentials, but is unavailable in Firefox and cannot be told about
//!   project-specific words.
//!
//! When neither is available the button renders greyed-out. Hovering shows a
//! native tooltip and clicking pops a short hint explaining why.
//!
//! The iOS workarounds below apply to the **browser** path only — the
//! recorder has no singleton constraint and no prompt race, which is a second
//! reason to prefer the server path where it is available.
//!
//! iOS-specific behavior (see #840): iOS WebKit allows only one active
//! `SpeechRecognition` per page, and races the permission prompt against
//! `recognition.start()`. We work around this by (1) priming mic permission
//! via `getUserMedia({audio:true})` and holding the stream open for the
//! recognizer's lifetime, and (2) tracking a `pending_stop` flag so a new
//! session can't start until the previous one's `onend` has fired.

use gloo::timers::callback::Timeout;
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use shared::api::TranscriptionResponse;
use uuid::Uuid;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Blob, BlobEvent, MediaRecorder, MediaRecorderOptions, MediaStream};
use yew::prelude::*;

use crate::utils;

const UNSUPPORTED_HINT: &str = "Voice input needs the Web Speech API. Try Chrome, Edge, or Safari.";
const BUSY_HINT: &str = "Voice recognizer is busy — wait a moment and tap again.";
const MIC_DENIED_HINT: &str =
    "Microphone permission was denied. Enable it in your browser settings and try again.";
const RECORDING_UNSUPPORTED_HINT: &str =
    "This browser cannot record audio, so voice input is unavailable.";

/// Container preferences for the recording, best first. Chromium and Firefox
/// take Opus in WebM; Safari records MP4/AAC. An empty tail lets the browser
/// pick when it likes none of these.
const PREFERRED_MIME_TYPES: &[&str] = &["audio/webm;codecs=opus", "audio/webm", "audio/mp4"];

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
    /// Record locally and transcribe on the server rather than using the
    /// browser's recognizer. Set from `AppConfig::stt_enabled`.
    #[prop_or(false)]
    pub server_stt: bool,
    /// Session the recording belongs to. Sent with the audio so the server can
    /// bias the recognizer toward this session's vocabulary; without it the
    /// transcript is still returned, just less accurate on project-specific
    /// words.
    #[prop_or_default]
    pub session_id: Option<Uuid>,
}

pub enum VoiceInputMsg {
    ToggleRecording,
    SessionStarted(ActiveSession),
    /// The recorder is live (server path).
    RecordingStarted(ServerRecording),
    /// The recorder flushed its final chunk: `(audio, content type)`.
    AudioReady(Vec<u8>, String),
    /// The server returned a transcript. Empty means silence, which is not an
    /// error — the user simply said nothing.
    Transcribed(String),
    TranscribeFailed(String),
    StartFailed(StartFailure),
    Final(String),
    Interim(String),
    /// `(kind, message)` — `event.error` and the full `event.message` text.
    /// `message` is needed to discriminate iOS's `aborted/"Another request is
    /// started"` from a plain user-initiated abort.
    RecognitionError(String, String),
    Ended,
    /// Fallback fired by the stop-watchdog when iOS doesn't deliver `onend`
    /// within a reasonable window after `stop()`. Idempotent with `Ended`.
    StopWatchdog,
    /// Max-duration safety stop. Fires once after `MAX_SESSION_MS` even if
    /// the recognizer is still alive — prevents a leaked session from holding
    /// the mic forever.
    MaxDurationReached,
    HideHint,
}

/// How long after `recognition.stop()` we wait for iOS's `onend` before
/// force-clearing `pending_stop` ourselves. 1.5s comfortably covers a normal
/// teardown (~tens of ms on desktop) but unblocks the user when iOS hangs.
const STOP_WATCHDOG_MS: u32 = 1500;

/// Hard ceiling on a single recording session. After this elapsed, we trigger
/// a stop even if the user hasn't tapped — defense against a stuck recognizer
/// holding the mic indicator on iOS.
const MAX_SESSION_MS: u32 = 60_000;

/// How long to wait after `MediaRecorder::stop()` for the final chunk before
/// declaring the recording lost. Assembling a blob is fast; this only has to
/// beat a wedged recorder, and it deliberately does not cover the upload,
/// which can legitimately take seconds.
const RECORDER_FLUSH_WATCHDOG_MS: u32 = 3000;

/// Reasons the async start path can bail before a session is established.
pub enum StartFailure {
    /// The user denied mic permission (or the browser blocked it).
    PermissionDenied,
    /// Anything else — surfaced to the parent via `on_error`.
    Other(String),
}

/// The in-flight capture. Only one strategy is ever active — which one is
/// fixed by `server_stt` — so making them alternatives rather than two
/// independent slots keeps "recording" a single question.
enum Capture {
    /// Browser-native recognition.
    Browser(ActiveSession),
    /// Local recording, transcribed by the server.
    Server(ServerRecording),
}

/// Owns the `MediaRecorder`, the microphone stream and the event closures for
/// one recording, plus the chunks delivered so far.
///
/// Dropping it stops the recorder and releases the microphone. Note the
/// ordering constraint that follows: after `stop()` the browser still has to
/// deliver `dataavailable` and `stop`, so this must stay alive until the audio
/// has actually arrived — see `VoiceInputMsg::AudioReady`.
pub struct ServerRecording {
    recorder: MediaRecorder,
    mic_stream: JsValue,
    /// Container the browser actually chose, which is what the upload declares.
    mime_type: String,
    /// Chunks delivered by `dataavailable`, assembled on stop.
    chunk_store: Array,
    _on_data: Closure<dyn FnMut(BlobEvent)>,
    _on_stop: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
}

impl Drop for ServerRecording {
    fn drop(&mut self) {
        // `stop()` on an already-inactive recorder throws; ignore it, the point
        // is only that it is not left running.
        let _ = self.recorder.stop();
        stop_media_stream(&self.mic_stream);
    }
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

impl ActiveSession {
    /// Ask the recognizer to finish, which eventually fires `onend`.
    ///
    /// `Drop` does this too, as a safety net; calling it explicitly keeps the
    /// two capture paths symmetric and makes the teardown visible where it
    /// happens rather than implied by a scope ending.
    fn request_stop(&self) {
        if let Ok(stop) = Reflect::get(&self.recognition, &JsValue::from_str("stop")) {
            if let Ok(stop_fn) = stop.dyn_into::<Function>() {
                let _ = stop_fn.call0(&self.recognition);
            }
        }
    }
}

impl Drop for ActiveSession {
    fn drop(&mut self) {
        self.request_stop();
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
    /// `true` from the moment a recording stops until its transcript comes
    /// back. The server path has no interim results, so this is what tells the
    /// user something is still happening.
    transcribing: bool,
    capture: Option<Capture>,
    hint_message: Option<&'static str>,
    hint_timer: Option<Timeout>,
    /// Watchdog set when we call `stop()`; clears `pending_stop` if `onend`
    /// is late. Cleared on `Ended`.
    stop_watchdog: Option<Timeout>,
    /// Max-duration timer set when a session is established. Triggers
    /// `MaxDurationReached` after `MAX_SESSION_MS`. Cleared on `Ended`.
    max_duration_timer: Option<Timeout>,
}

impl Component for VoiceInput {
    type Message = VoiceInputMsg;
    type Properties = VoiceInputProps;

    fn create(ctx: &Context<Self>) -> Self {
        Self {
            supported: if ctx.props().server_stt {
                MediaRecorder::is_type_supported("audio/webm")
                    || MediaRecorder::is_type_supported("audio/mp4")
            } else {
                is_speech_recognition_supported()
            },
            is_recording: false,
            is_starting: false,
            pending_stop: false,
            transcribing: false,
            capture: None,
            hint_message: None,
            hint_timer: None,
            stop_watchdog: None,
            max_duration_timer: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            VoiceInputMsg::ToggleRecording => {
                if !self.supported {
                    self.show_hint(ctx, UNSUPPORTED_HINT);
                    return true;
                }
                if self.pending_stop || self.is_starting || self.transcribing {
                    self.show_hint(ctx, BUSY_HINT);
                    return true;
                }

                if self.is_recording {
                    self.stop_capture(ctx);
                    return true;
                }

                // Optimistic UI: light up the recording icon immediately so
                // the user has feedback while we wait for getUserMedia + start.
                self.is_recording = true;
                self.is_starting = true;
                ctx.props().on_recording_change.emit(true);

                let link = ctx.link().clone();
                if ctx.props().server_stt {
                    spawn_local(async move {
                        match start_recording_async().await {
                            Ok(recording) => {
                                link.send_message(VoiceInputMsg::RecordingStarted(recording));
                            }
                            Err(failure) => {
                                link.send_message(VoiceInputMsg::StartFailed(failure));
                            }
                        }
                    });
                } else {
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
                }
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
                self.capture = Some(Capture::Browser(session));
                self.arm_max_duration(ctx);
                true
            }
            VoiceInputMsg::RecordingStarted(recording) => {
                self.is_starting = false;
                if !self.is_recording {
                    // Toggled off (or errored) while the permission primer was
                    // still resolving; drop it so the mic is released.
                    drop(recording);
                    return true;
                }
                let link = ctx.link().clone();
                recording.attach_stop_handler(link);
                self.capture = Some(Capture::Server(recording));
                self.arm_max_duration(ctx);
                true
            }
            VoiceInputMsg::AudioReady(audio, content_type) => {
                self.stop_watchdog = None;
                // The recorder has delivered everything; release the mic now
                // rather than holding it for the length of the upload.
                self.capture = None;

                if audio.is_empty() {
                    self.transcribing = false;
                    return true;
                }

                let link = ctx.link().clone();
                let session_id = ctx.props().session_id;
                spawn_local(async move {
                    match transcribe(session_id, audio, content_type).await {
                        Ok(text) => link.send_message(VoiceInputMsg::Transcribed(text)),
                        Err(error) => link.send_message(VoiceInputMsg::TranscribeFailed(error)),
                    }
                });
                true
            }
            VoiceInputMsg::Transcribed(text) => {
                self.transcribing = false;
                let trimmed = text.trim();
                // Silence transcribes to nothing; that is not an error, and
                // surfacing one for it would be noise.
                if !trimmed.is_empty() {
                    ctx.props().on_transcription.emit(trimmed.to_string());
                }
                true
            }
            VoiceInputMsg::TranscribeFailed(error) => {
                self.transcribing = false;
                log::warn!("Transcription failed: {}", error);
                ctx.props().on_error.emit(error);
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
                self.capture = None;
                if self.is_recording {
                    self.is_recording = false;
                    self.pending_stop = true;
                    ctx.props().on_recording_change.emit(false);
                }
                true
            }
            VoiceInputMsg::Ended => {
                self.capture = None;
                self.pending_stop = false;
                self.stop_watchdog = None;
                self.max_duration_timer = None;
                if self.is_recording {
                    self.is_recording = false;
                    ctx.props().on_recording_change.emit(false);
                }
                true
            }
            VoiceInputMsg::StopWatchdog => {
                self.stop_watchdog = None;
                // Server path: the recorder never delivered its final chunk,
                // so there is nothing to upload and the mic is still held.
                if matches!(self.capture, Some(Capture::Server(_))) {
                    log::warn!(
                        "MediaRecorder.stop() didn't deliver audio within {}ms",
                        RECORDER_FLUSH_WATCHDOG_MS
                    );
                    self.capture = None;
                    self.transcribing = false;
                    self.max_duration_timer = None;
                    ctx.props()
                        .on_error
                        .emit("Recording did not finish — please try again.".to_string());
                    return true;
                }
                if self.pending_stop {
                    log::warn!(
                        "SpeechRecognition.stop() didn't deliver onend within \
                         {}ms — force-clearing pending_stop",
                        STOP_WATCHDOG_MS
                    );
                    self.pending_stop = false;
                    self.capture = None;
                    self.max_duration_timer = None;
                    true
                } else {
                    false
                }
            }
            VoiceInputMsg::MaxDurationReached => {
                self.max_duration_timer = None;
                if self.capture.is_some() {
                    log::warn!(
                        "Voice session exceeded {}ms — auto-stopping",
                        MAX_SESSION_MS
                    );
                    self.stop_capture(ctx);
                    true
                } else {
                    false
                }
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
            self.transcribing.then_some("transcribing"),
            (!self.supported).then_some("unsupported"),
        );

        let title = if !self.supported {
            if ctx.props().server_stt {
                RECORDING_UNSUPPORTED_HINT
            } else {
                UNSUPPORTED_HINT
            }
        } else if self.transcribing {
            "Transcribing…"
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
                    } else if self.transcribing {
                        // The server path has no interim text, so the button
                        // itself has to show that work is still in flight.
                        <span class="voice-icon transcribing-icon">{ "\u{22EF}" }</span>
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
        self.capture = None;
        self.hint_timer = None;
        self.stop_watchdog = None;
        self.max_duration_timer = None;
    }
}

impl VoiceInput {
    /// End the in-flight capture, whichever strategy it is.
    fn stop_capture(&mut self, ctx: &Context<Self>) {
        let link = ctx.link().clone();
        match self.capture.take() {
            Some(Capture::Server(recording)) => {
                // Ask for the final chunk and put the recording *back*: the
                // browser still has to fire `stop`, and dropping now would
                // detach the handler that hands us the audio.
                recording.request_stop();
                self.capture = Some(Capture::Server(recording));
                self.transcribing = true;
                self.stop_watchdog = Some(Timeout::new(RECORDER_FLUSH_WATCHDOG_MS, move || {
                    link.send_message(VoiceInputMsg::StopWatchdog);
                }));
            }
            // Browser path: the recognizer's `onend` clears `pending_stop`.
            // iOS sometimes never fires it, hence the watchdog.
            other => {
                if let Some(Capture::Browser(session)) = &other {
                    session.request_stop();
                }
                self.pending_stop = true;
                self.stop_watchdog = Some(Timeout::new(STOP_WATCHDOG_MS, move || {
                    link.send_message(VoiceInputMsg::StopWatchdog);
                }));
            }
        }
        self.is_recording = false;
        self.max_duration_timer = None;
        ctx.props().on_recording_change.emit(false);
    }

    /// Arm the safety stop that keeps a wedged capture from holding the mic.
    fn arm_max_duration(&mut self, ctx: &Context<Self>) {
        let link = ctx.link().clone();
        self.max_duration_timer = Some(Timeout::new(MAX_SESSION_MS, move || {
            link.send_message(VoiceInputMsg::MaxDurationReached);
        }));
    }

    fn show_hint(&mut self, ctx: &Context<Self>, message: &'static str) {
        self.hint_message = Some(message);
        let link = ctx.link().clone();
        self.hint_timer = Some(Timeout::new(4000, move || {
            link.send_message(VoiceInputMsg::HideHint);
        }));
    }
}

impl ServerRecording {
    /// Ask the recorder for its final chunk. Safe to call once; a second call
    /// on an inactive recorder throws and is ignored.
    fn request_stop(&self) {
        let _ = self.recorder.stop();
    }

    /// Wire up `onstop` now that the component can receive the result.
    ///
    /// Deliberately not done at construction: the handler needs the component
    /// link, and attaching it here keeps `start_recording_async` free of any
    /// dependency on the component being ready.
    fn attach_stop_handler(&self, link: yew::html::Scope<VoiceInput>) {
        let chunks = self.chunks();
        let mime_type = self.mime_type.clone();
        let on_stop = Closure::wrap(Box::new(move |_event: JsValue| {
            let link = link.clone();
            let mime_type = mime_type.clone();
            let parts = chunks.clone();
            spawn_local(async move {
                let (audio, content_type) = collect_audio(&parts, &mime_type).await;
                link.send_message(VoiceInputMsg::AudioReady(audio, content_type));
            });
        }) as Box<dyn FnMut(JsValue)>);
        self.recorder
            .set_onstop(Some(on_stop.as_ref().unchecked_ref()));
        // Leak the closure: it must outlive this call, and the recorder is
        // torn down immediately after `onstop` fires exactly once.
        on_stop.forget();
    }

    fn chunks(&self) -> Array {
        self.chunk_store.clone()
    }
}

/// Assemble the recorded chunks into one buffer.
///
/// The blob's own type is preferred over the requested one: the browser is
/// free to record something other than what was asked for, and the upload has
/// to declare what was actually produced.
async fn collect_audio(chunks: &Array, fallback_mime: &str) -> (Vec<u8>, String) {
    let Ok(blob) = Blob::new_with_blob_sequence(chunks) else {
        return (Vec::new(), fallback_mime.to_string());
    };
    let content_type = match blob.type_() {
        t if t.is_empty() => fallback_mime.to_string(),
        t => t,
    };
    let Ok(buffer) = JsFuture::from(blob.array_buffer()).await else {
        return (Vec::new(), content_type);
    };
    (Uint8Array::new(&buffer).to_vec(), content_type)
}

/// The container to ask the recorder for: the first the browser admits to
/// supporting, or `None` to let it choose.
fn preferred_mime_type() -> Option<&'static str> {
    PREFERRED_MIME_TYPES
        .iter()
        .copied()
        .find(|candidate| MediaRecorder::is_type_supported(candidate))
}

/// Start recording: acquire the microphone, then build a recorder over it.
///
/// Unlike the browser path this has no permission race to work around — the
/// recorder is constructed from a stream we already hold.
async fn start_recording_async() -> Result<ServerRecording, StartFailure> {
    let mic_stream = request_mic_stream().await.map_err(|name| {
        log::warn!("getUserMedia rejected: {}", name);
        if name == "NotAllowedError" || name == "PermissionDeniedError" {
            StartFailure::PermissionDenied
        } else {
            StartFailure::Other(format!("Could not access microphone: {}", name))
        }
    })?;

    let stream = mic_stream.clone().dyn_into::<MediaStream>().map_err(|_| {
        stop_media_stream(&mic_stream);
        StartFailure::Other("Microphone did not yield a media stream".into())
    })?;

    let requested_mime = preferred_mime_type();
    let recorder = match requested_mime {
        Some(mime) => {
            let options = MediaRecorderOptions::new();
            options.set_mime_type(mime);
            MediaRecorder::new_with_media_stream_and_media_recorder_options(&stream, &options)
        }
        None => MediaRecorder::new_with_media_stream(&stream),
    }
    .map_err(|e| {
        stop_media_stream(&mic_stream);
        StartFailure::Other(format!("Could not start recording: {:?}", e))
    })?;

    let chunk_store = Array::new();
    let sink = chunk_store.clone();
    let on_data = Closure::wrap(Box::new(move |event: BlobEvent| {
        if let Some(blob) = event.data() {
            // Zero-length chunks show up on some browsers when a recording is
            // stopped immediately; they contribute nothing to the blob.
            if blob.size() > 0.0 {
                sink.push(&blob);
            }
        }
    }) as Box<dyn FnMut(BlobEvent)>);
    recorder.set_ondataavailable(Some(on_data.as_ref().unchecked_ref()));

    let on_error = Closure::wrap(Box::new(move |event: JsValue| {
        log::warn!("MediaRecorder error: {:?}", event);
    }) as Box<dyn FnMut(JsValue)>);
    recorder.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    // Placeholder until `attach_stop_handler` installs the real one.
    let on_stop = Closure::wrap(Box::new(|_event: JsValue| {}) as Box<dyn FnMut(JsValue)>);

    if let Err(e) = recorder.start() {
        stop_media_stream(&mic_stream);
        return Err(StartFailure::Other(format!(
            "Could not start recording: {:?}",
            e
        )));
    }

    let mime_type = match recorder.mime_type() {
        t if !t.is_empty() => t,
        _ => requested_mime.unwrap_or("audio/webm").to_string(),
    };

    Ok(ServerRecording {
        recorder,
        mic_stream,
        mime_type,
        chunk_store,
        _on_data: on_data,
        _on_stop: on_stop,
        _on_error: on_error,
    })
}

/// POST one recording to the server and return the transcript.
async fn transcribe(
    session_id: Option<Uuid>,
    audio: Vec<u8>,
    content_type: String,
) -> Result<String, String> {
    let path = transcribe_path(session_id, document_language().as_deref());
    let body = Uint8Array::from(audio.as_slice());
    let response = gloo_net::http::Request::post(&utils::api_url(&path))
        .header("Content-Type", &content_type)
        .body(body)
        .map_err(|e| format!("Could not build the upload: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Could not reach the server: {e}"))?;

    if !response.ok() {
        return Err(upload_error_message(response.status()));
    }

    response
        .json::<TranscriptionResponse>()
        .await
        .map(|body| body.text)
        .map_err(|e| format!("Could not read the transcript: {e}"))
}

/// Build the upload path, including whatever context we can supply.
///
/// Both parameters are optional: without a session the server just skips
/// vocabulary biasing, and without a language it lets the provider decide.
fn transcribe_path(session_id: Option<Uuid>, language: Option<&str>) -> String {
    let mut params: Vec<String> = Vec::new();
    if let Some(session_id) = session_id {
        params.push(format!("session_id={session_id}"));
    }
    if let Some(language) = language.filter(|l| !l.is_empty()) {
        params.push(format!("language={language}"));
    }
    if params.is_empty() {
        "/api/stt/transcribe".to_string()
    } else {
        format!("/api/stt/transcribe?{}", params.join("&"))
    }
}

/// Turn an upload failure into something the user can act on.
///
/// The two cases worth naming are a server with no provider configured and a
/// recording over the size cap; everything else is a bare status, which is at
/// least enough to match against the server log.
fn upload_error_message(status: u16) -> String {
    match status {
        503 => "Speech-to-text is not configured on this server.".to_string(),
        413 => "That recording is too long to transcribe.".to_string(),
        401 => "Your session expired — reload and try again.".to_string(),
        status => format!("Transcription failed (HTTP {status})."),
    }
}

/// The document's declared language, used to tell the recognizer what to
/// expect. Shared with the browser path, which reads the same attribute.
fn document_language() -> Option<String> {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
        .and_then(|el| el.get_attribute("lang"))
        .filter(|lang| !lang.is_empty())
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
    // continuous=false: single-utterance per tap. With true, iOS Safari
    // never auto-ends and our SessionView's "auto-send on Final" already
    // implies a one-tap-one-utterance UX anyway. See #840 follow-up.
    set_bool("continuous", false);
    set_bool("interimResults", true);

    let lang = document_language().unwrap_or_else(|| "en-US".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upload_path_carries_session_and_language() {
        let id = Uuid::nil();
        assert_eq!(
            transcribe_path(Some(id), Some("en-US")),
            format!("/api/stt/transcribe?session_id={id}&language=en-US")
        );
    }

    /// Both are optional; the server degrades rather than rejecting.
    #[test]
    fn the_upload_path_omits_what_it_does_not_know() {
        assert_eq!(transcribe_path(None, None), "/api/stt/transcribe");
        assert_eq!(
            transcribe_path(None, Some("fr-FR")),
            "/api/stt/transcribe?language=fr-FR"
        );
        let id = Uuid::nil();
        assert_eq!(
            transcribe_path(Some(id), None),
            format!("/api/stt/transcribe?session_id={id}")
        );
    }

    /// An empty `lang` attribute is the same as none — sending `language=`
    /// would have the server try to honor an empty tag.
    #[test]
    fn an_empty_language_is_dropped() {
        assert_eq!(transcribe_path(None, Some("")), "/api/stt/transcribe");
    }

    #[test]
    fn upload_failures_name_the_actionable_cases() {
        assert!(upload_error_message(503).contains("not configured"));
        assert!(upload_error_message(413).contains("too long"));
        assert!(upload_error_message(401).contains("expired"));
        // Anything else still carries the status so it can be correlated with
        // the server log.
        assert!(upload_error_message(500).contains("500"));
    }
}

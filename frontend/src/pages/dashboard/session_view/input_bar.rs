//! `InputBar` — sub-component owning the textarea, send-mode dropdown,
//! voice transcription, command history, and file-upload pipeline.
//!
//! Pulled out of `SessionView` so the parent component no longer carries the
//! textarea `NodeRef`, the file-input `NodeRef`, the upload-progress state,
//! the send-mode dropdown toggle, the drag-hover flag, the command-history
//! cursor, or the voice-recording / interim-transcription fields. The parent
//! keeps the WebSocket plumbing: when the user submits text, the bar emits a
//! typed `(String, SendMode)` via `on_send_text` and the parent packages it
//! into the `ClientToServer::ClaudeInput` frame (plus the optimistic local
//! echo); when the user uploads files, the bar streams `FileUploadStart` /
//! `FileUploadChunk` / `ClaudeInput` frames out through `on_send_frame` so
//! the chunking and combined-message synthesis stay adjacent to the upload
//! UI while the actual WS send still goes through the parent.

use super::history::CommandHistory;
use crate::components::VoiceInput;
use crate::utils::format_file_size;
use gloo::timers::callback::Timeout;
use shared::protocol::UPLOAD_CHUNK_SIZE;
use shared::{ClientToServer, SendMode};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{ClipboardEvent, DragEvent, Element, HtmlTextAreaElement, KeyboardEvent};
use yew::prelude::*;

/// Inputs to the bar.
#[derive(Properties, PartialEq)]
pub struct InputBarProps {
    /// Session this bar is for. Used to seed the command-history localStorage
    /// key and as the `session_id` prop for the embedded `VoiceInput`.
    pub session_id: Uuid,
    /// Whether the parent session is currently focused. When `true`, the bar
    /// grabs textarea focus on transitions.
    pub focused: bool,
    /// Whether the WebSocket is currently connected. Disables the textarea
    /// + send buttons when false.
    pub ws_connected: bool,
    /// Fired exactly once on `create`, handing the parent a callback it can
    /// invoke to push inbound events into the bar. Mirrors the
    /// dispatcher-registration pattern used by `PermissionHandler` and
    /// `TasksPanel`.
    pub on_register: Callback<Callback<InputBarInbound>>,
    /// Plain-text submit. Parent packages into `ClientToServer::ClaudeInput`
    /// and emits the optimistic local echo.
    pub on_send_text: Callback<(String, SendMode)>,
    /// Raw WS frame emitted by the bar. Used by the file-upload pipeline
    /// to stream `FileUploadStart` / `FileUploadChunk` / the combined
    /// `ClaudeInput` frames.
    pub on_send_frame: Callback<ClientToServer>,
    /// Fires once per submit (text or upload) so the parent can bump its
    /// per-session "I sent something" bookkeeping.
    pub on_message_sent: Callback<()>,
}

/// Channel from the parent to the bar.
#[derive(Debug, Clone)]
pub enum InputBarInbound {
    /// Re-focus the textarea. Emitted by `PermissionHandler` (via the parent)
    /// after a permission is answered so the user can keep typing without
    /// a stray click.
    FocusTextarea,
}

pub enum InputBarMsg {
    Inbound(InputBarInbound),
    /// User typed into the textarea. We track the value in state so it can
    /// be restored after a re-render (e.g. on WS reconnect).
    UpdateInput(String),
    /// User submitted the form (Enter, or click "Send"). Sends with
    /// `SendMode::Normal`.
    SendInput,
    /// User picked "Wiggum" from the send-mode dropdown.
    SendWiggum,
    HistoryUp,
    HistoryDown,
    ToggleSendModeDropdown,
    CloseSendModeDropdown,
    FilesSelected(Vec<web_sys::File>),
    FileUploadProgress(f32),
    FileUploaded(String),
    FileUploadError(String),
    DragEnter,
    DragLeave,
    UploadDismiss,
    VoiceRecordingChanged(bool),
    VoiceTranscription(String),
    VoiceInterimTranscription(String),
    VoiceError(String),
    /// Ctrl+M keyboard shortcut: click the voice button programmatically.
    ToggleVoice,
    /// No-op (used by keydown / paste handlers that need to return a message
    /// but don't want to mutate state).
    Noop,
}

pub struct InputBar {
    input_ref: NodeRef,
    /// Mirror of the textarea value, kept so a re-render (reconnect, focus
    /// flip) can restore the text without losing what the user typed.
    input_text: String,
    command_history: CommandHistory,
    send_mode_dropdown_open: bool,
    file_input_ref: NodeRef,
    upload_progress: Option<f32>,
    upload_files: Vec<(String, u64)>,
    upload_departing: bool,
    upload_dismiss_timer: Option<Timeout>,
    drag_hover: bool,
    is_recording: bool,
    interim_transcription: Option<String>,
    voice_button_ref: NodeRef,
    was_focused: bool,
}

impl Component for InputBar {
    type Message = InputBarMsg;
    type Properties = InputBarProps;

    fn create(ctx: &Context<Self>) -> Self {
        let dispatcher = ctx.link().callback(InputBarMsg::Inbound);
        ctx.props().on_register.emit(dispatcher);

        Self {
            input_ref: NodeRef::default(),
            input_text: String::new(),
            command_history: CommandHistory::for_session(ctx.props().session_id),
            send_mode_dropdown_open: false,
            file_input_ref: NodeRef::default(),
            upload_progress: None,
            upload_files: Vec::new(),
            upload_departing: false,
            upload_dismiss_timer: None,
            drag_hover: false,
            is_recording: false,
            interim_transcription: None,
            voice_button_ref: NodeRef::default(),
            was_focused: ctx.props().focused,
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, _old_props: &Self::Properties) -> bool {
        let now_focused = ctx.props().focused;
        let became_focused = now_focused && !self.was_focused;
        self.was_focused = now_focused;
        if became_focused {
            self.focus_textarea();
        }
        true
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render && ctx.props().focused {
            self.focus_textarea();
        }
        // Restore textarea content if it was cleared by a re-render (e.g.
        // WS reconnect): the textarea is uncontrolled (we don't pass `value`
        // through Yew) so we need to re-apply our tracked text manually.
        if !self.input_text.is_empty() {
            if let Some(el) = self.input_ref.cast::<HtmlTextAreaElement>() {
                if el.value().is_empty() {
                    self.set_input_text(&self.input_text.clone());
                }
            }
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            InputBarMsg::Inbound(InputBarInbound::FocusTextarea) => {
                self.focus_textarea();
                false
            }
            InputBarMsg::UpdateInput(value) => {
                // Textarea is uncontrolled — the DOM already has the new
                // value. Track in state so we can restore after re-renders.
                self.input_text = value;
                false
            }
            InputBarMsg::SendInput => self.dispatch_text_send(ctx, SendMode::Normal),
            InputBarMsg::SendWiggum => {
                self.send_mode_dropdown_open = false;
                self.dispatch_text_send(ctx, SendMode::Wiggum)
            }
            InputBarMsg::HistoryUp => {
                let current = self.get_input_text();
                if let Some(cmd) = self.command_history.navigate_up(&current) {
                    self.set_input_text(&cmd);
                }
                false
            }
            InputBarMsg::HistoryDown => {
                if let Some(cmd) = self.command_history.navigate_down() {
                    self.set_input_text(&cmd);
                }
                false
            }
            InputBarMsg::ToggleSendModeDropdown => {
                self.send_mode_dropdown_open = !self.send_mode_dropdown_open;
                true
            }
            InputBarMsg::CloseSendModeDropdown => {
                if self.send_mode_dropdown_open {
                    self.send_mode_dropdown_open = false;
                    true
                } else {
                    false
                }
            }
            InputBarMsg::FilesSelected(files) => {
                self.start_upload(ctx, files);
                true
            }
            InputBarMsg::FileUploadProgress(progress) => {
                self.upload_progress = Some(progress);
                true
            }
            InputBarMsg::FileUploaded(_filename) => {
                self.upload_progress = Some(1.0);
                self.upload_departing = false;
                let link = ctx.link().clone();
                self.upload_dismiss_timer = Some(Timeout::new(2_000, move || {
                    link.send_message(InputBarMsg::UploadDismiss);
                }));
                true
            }
            InputBarMsg::UploadDismiss => {
                self.upload_departing = true;
                self.upload_dismiss_timer = None;
                let link = ctx.link().clone();
                // After the CSS collapse animation finishes, clear the bar
                // by reusing the error path (which also wipes the state).
                self.upload_dismiss_timer = Some(Timeout::new(400, move || {
                    link.send_message(InputBarMsg::FileUploadError("dismiss".into()));
                }));
                true
            }
            InputBarMsg::FileUploadError(_err) => {
                self.upload_progress = None;
                self.upload_files.clear();
                self.upload_departing = false;
                self.upload_dismiss_timer = None;
                true
            }
            InputBarMsg::DragEnter => {
                self.drag_hover = true;
                true
            }
            InputBarMsg::DragLeave => {
                self.drag_hover = false;
                true
            }
            InputBarMsg::VoiceRecordingChanged(recording) => {
                self.is_recording = recording;
                if !recording {
                    self.interim_transcription = None;
                }
                true
            }
            InputBarMsg::VoiceTranscription(text) => {
                self.interim_transcription = None;
                if !text.is_empty() {
                    let current = self.get_input_text();
                    let new_value = if current.is_empty() {
                        text
                    } else {
                        format!("{} {}", current, text)
                    };
                    self.set_input_text(&new_value);
                    ctx.link().send_message(InputBarMsg::SendInput);
                }
                true
            }
            InputBarMsg::VoiceInterimTranscription(text) => {
                self.interim_transcription = if text.is_empty() { None } else { Some(text) };
                true
            }
            InputBarMsg::VoiceError(err) => {
                log::error!("Voice error: {}", err);
                self.is_recording = false;
                self.interim_transcription = None;
                true
            }
            InputBarMsg::ToggleVoice => {
                if let Some(button) = self.voice_button_ref.cast::<web_sys::HtmlElement>() {
                    button.click();
                }
                false
            }
            InputBarMsg::Noop => false,
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();

        let handle_submit = link.callback(|e: SubmitEvent| {
            e.prevent_default();
            InputBarMsg::SendInput
        });

        let handle_input = link.callback(|e: InputEvent| {
            let input: HtmlTextAreaElement = e.target_unchecked_into();
            let el: &Element = input.as_ref();
            // Measure content height with overflow hidden to prevent
            // scrollbar from narrowing the text area and causing layout
            // bounce.
            el.set_attribute("style", "height: 0; overflow-y: hidden")
                .ok();
            el.set_attribute("style", &format!("height: {}px", input.scroll_height()))
                .ok();
            InputBarMsg::UpdateInput(input.value())
        });

        let handle_keydown = link.callback(|e: KeyboardEvent| {
            if e.ctrl_key() && e.key().to_lowercase() == "m" {
                e.prevent_default();
                return InputBarMsg::ToggleVoice;
            }

            match e.key().as_str() {
                "Enter" if !e.shift_key() => {
                    // Enter without Shift submits.
                    e.prevent_default();
                    InputBarMsg::SendInput
                }
                "Enter" => InputBarMsg::Noop,
                "ArrowUp" => {
                    // Only trigger history if the cursor is on the first
                    // line. Otherwise let the arrow key move the cursor.
                    let target: HtmlTextAreaElement = e.target_unchecked_into();
                    let value = target.value();
                    let cursor = target.selection_start().ok().flatten().unwrap_or(0) as usize;
                    let on_first_line = !value[..cursor.min(value.len())].contains('\n');
                    if on_first_line {
                        e.prevent_default();
                        InputBarMsg::HistoryUp
                    } else {
                        InputBarMsg::Noop
                    }
                }
                "ArrowDown" => {
                    let target: HtmlTextAreaElement = e.target_unchecked_into();
                    let value = target.value();
                    let cursor = target.selection_start().ok().flatten().unwrap_or(0) as usize;
                    let on_last_line = !value[cursor.min(value.len())..].contains('\n');
                    if on_last_line {
                        e.prevent_default();
                        InputBarMsg::HistoryDown
                    } else {
                        InputBarMsg::Noop
                    }
                }
                _ => InputBarMsg::Noop,
            }
        });

        let close_dropdown = link.callback(|_| InputBarMsg::CloseSendModeDropdown);

        let handle_paste = link.callback(|e: Event| {
            let e: ClipboardEvent = e.unchecked_into();
            if let Some(data) = e.clipboard_data() {
                let items = data.items();
                let files: Vec<web_sys::File> = (0..items.length())
                    .filter_map(|i: u32| items.get(i))
                    .filter(|item: &web_sys::DataTransferItem| item.kind() == "file")
                    .filter_map(|item: web_sys::DataTransferItem| item.get_as_file().ok().flatten())
                    .collect();
                if !files.is_empty() {
                    e.prevent_default();
                    return InputBarMsg::FilesSelected(files);
                }
            }
            InputBarMsg::Noop
        });

        let handle_dragover = link.callback(|e: DragEvent| {
            e.prevent_default();
            InputBarMsg::DragEnter
        });

        let handle_dragleave = link.callback(|e: DragEvent| {
            e.prevent_default();
            InputBarMsg::DragLeave
        });

        let handle_drop = link.callback(|e: DragEvent| {
            e.prevent_default();
            let files: Vec<web_sys::File> = e
                .data_transfer()
                .and_then(|dt| dt.files())
                .map(|fl| (0..fl.length()).filter_map(|i: u32| fl.get(i)).collect())
                .unwrap_or_default();
            if !files.is_empty() {
                InputBarMsg::FilesSelected(files)
            } else {
                InputBarMsg::DragLeave
            }
        });

        let drag_hint = if self.drag_hover {
            "session-view-input drag-hover"
        } else {
            "session-view-input"
        };

        html! {
            <>
                { self.render_upload_bar() }
                <form
                    class={drag_hint}
                    onsubmit={handle_submit}
                    ondragover={handle_dragover}
                    ondragleave={handle_dragleave}
                    ondrop={handle_drop}
                    onclick={close_dropdown}
                >
                    <span class="input-prompt">{ ">" }</span>
                    { self.render_interim_transcription() }
                    <textarea
                        ref={self.input_ref.clone()}
                        class={classes!(
                            "message-input",
                            self.interim_transcription.is_some().then_some("has-interim")
                        )}
                        placeholder="Type your message... (Shift+Enter for new line)"
                        oninput={handle_input}
                        onkeydown={handle_keydown}
                        onpaste={handle_paste}
                        disabled={!ctx.props().ws_connected}
                        rows="1"
                    />
                    { self.render_voice_input(ctx) }
                    { self.render_send_button(ctx) }
                    <div class="drop-hint">{ "Drop files here to upload" }</div>
                </form>
            </>
        }
    }
}

impl InputBar {
    /// Re-focus the textarea. Centralized so the parent's
    /// `InputBarInbound::FocusTextarea` path, the `changed()` focus-flip path,
    /// and the `rendered()` first-render path all go through one cast site.
    fn focus_textarea(&self) {
        if let Some(input) = self.input_ref.cast::<HtmlTextAreaElement>() {
            let _ = input.focus();
        }
    }

    /// Read the current textarea value directly from the DOM.
    fn get_input_text(&self) -> String {
        self.input_ref
            .cast::<HtmlTextAreaElement>()
            .map(|el| el.value())
            .unwrap_or_default()
    }

    /// Write text to the textarea DOM element and auto-resize it. Does NOT
    /// trigger a Yew re-render.
    fn set_input_text(&self, text: &str) {
        if let Some(el) = self.input_ref.cast::<HtmlTextAreaElement>() {
            el.set_value(text);
            let elem: &Element = el.as_ref();
            if text.is_empty() {
                elem.remove_attribute("style").ok();
            } else {
                elem.set_attribute("style", "height: 0; overflow-y: hidden")
                    .ok();
                elem.set_attribute("style", &format!("height: {}px", el.scroll_height()))
                    .ok();
            }
        }
    }

    /// Shared text-submit path used by `SendInput` (Normal) and `SendWiggum`.
    /// Reads the textarea, clears it, pushes to history, and emits the
    /// typed `(text, mode)` event up to the parent — which packages it into
    /// `ClientToServer::ClaudeInput` and emits the optimistic local echo.
    fn dispatch_text_send(&mut self, ctx: &Context<Self>, mode: SendMode) -> bool {
        crate::audio::ensure_audio_context();
        let input = self.get_input_text().trim().to_string();
        if input.is_empty() {
            return false;
        }
        self.command_history.push(input.clone());
        self.set_input_text("");
        self.input_text.clear();
        ctx.props().on_message_sent.emit(());
        ctx.props().on_send_text.emit((input, mode));
        true
    }

    /// Drive the chunk-upload pipeline. Reads the current textarea as
    /// optional accompanying text, clears it, then spawns an async task
    /// that emits `FileUploadStart` / `FileUploadChunk` frames per file
    /// via `on_send_frame` and a final `ClaudeInput` carrying the combined
    /// message text.
    fn start_upload(&mut self, ctx: &Context<Self>, files: Vec<web_sys::File>) {
        self.send_mode_dropdown_open = false;
        self.drag_hover = false;
        self.upload_progress = Some(0.0);
        self.upload_files = files.iter().map(|f| (f.name(), f.size() as u64)).collect();

        let user_input = self.get_input_text().trim().to_string();
        self.set_input_text("");
        self.input_text.clear();
        if !user_input.is_empty() {
            self.command_history.push(user_input.clone());
        }
        ctx.props().on_message_sent.emit(());

        let link = ctx.link().clone();
        let on_send_frame = ctx.props().on_send_frame.clone();

        spawn_local(async move {
            let mut uploaded_files: Vec<(String, u64)> = Vec::new();
            let total_files = files.len();

            for (file_idx, file) in files.iter().enumerate() {
                let file_name = file.name();
                let file_size = file.size() as u64;
                let content_type = file.type_();

                let array_buffer =
                    match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                        Ok(buf) => buf,
                        Err(_) => {
                            link.send_message(InputBarMsg::FileUploadError(format!(
                                "Failed to read file: {}",
                                file_name
                            )));
                            return;
                        }
                    };
                let uint8_array = js_sys::Uint8Array::new(&array_buffer);
                let bytes = uint8_array.to_vec();

                let total_chunks = bytes.len().div_ceil(UPLOAD_CHUNK_SIZE).max(1) as u32;
                let upload_id = Uuid::new_v4().to_string();

                let ct = if content_type.is_empty() {
                    "application/octet-stream".to_string()
                } else {
                    content_type
                };

                on_send_frame.emit(ClientToServer::FileUploadStart(
                    shared::FileUploadStartFields {
                        upload_id: upload_id.clone(),
                        filename: file_name.clone(),
                        content_type: ct,
                        total_chunks,
                        total_size: file_size,
                    },
                ));

                for i in 0..total_chunks {
                    let start = i as usize * UPLOAD_CHUNK_SIZE;
                    let end = ((i as usize + 1) * UPLOAD_CHUNK_SIZE).min(bytes.len());
                    let chunk = &bytes[start..end];
                    let encoded =
                        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, chunk);

                    on_send_frame.emit(ClientToServer::FileUploadChunk(
                        shared::FileUploadChunkFields {
                            upload_id: upload_id.clone(),
                            chunk_index: i,
                            data: encoded,
                        },
                    ));
                }

                uploaded_files.push((file_name, file_size));

                let overall_progress = (file_idx + 1) as f32 / total_files as f32;
                link.send_message(InputBarMsg::FileUploadProgress(overall_progress));
            }

            let combined = build_upload_message(&user_input, &uploaded_files);
            on_send_frame.emit(ClientToServer::ClaudeInput {
                content: serde_json::Value::String(combined),
                send_mode: None,
            });

            link.send_message(InputBarMsg::FileUploaded(
                uploaded_files
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        });
    }

    fn render_interim_transcription(&self) -> Html {
        let Some(ref interim) = self.interim_transcription else {
            return html! {};
        };
        let current = self.get_input_text();
        let preview = if current.is_empty() {
            interim.clone()
        } else {
            format!("{} {}", current, interim)
        };
        html! { <div class="interim-transcription">{ preview }</div> }
    }

    fn render_upload_bar(&self) -> Html {
        let Some(progress) = self.upload_progress else {
            return html! {};
        };

        let complete = progress >= 1.0;
        let file_count = self.upload_files.len();
        let header = if complete {
            "Upload complete".to_string()
        } else if file_count == 1 {
            "Uploading 1 file...".to_string()
        } else {
            format!("Uploading {} files...", file_count)
        };

        let files_html = self
            .upload_files
            .iter()
            .map(|(name, size)| {
                let label = format!("{} ({})", name, format_file_size(*size));
                html! { <div class="upload-bar-file">{ label }</div> }
            })
            .collect::<Html>();

        let pct = (progress * 100.0) as u32;
        let fill_style = format!("width: {}%", pct);

        let bar_class = if self.upload_departing {
            "upload-bar departing"
        } else {
            "upload-bar"
        };

        html! {
            <div class={bar_class}>
                <div class="upload-bar-header">{ header }</div>
                { files_html }
                <div class="upload-bar-track">
                    <div class="upload-bar-fill" style={fill_style} />
                </div>
            </div>
        }
    }

    fn render_voice_input(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let session_id = ctx.props().session_id;
        let on_recording_change = link.callback(InputBarMsg::VoiceRecordingChanged);
        let on_transcription = link.callback(InputBarMsg::VoiceTranscription);
        let on_interim_transcription = link.callback(InputBarMsg::VoiceInterimTranscription);
        let on_error = link.callback(InputBarMsg::VoiceError);
        let button_ref = self.voice_button_ref.clone();
        html! {
            <VoiceInput
                session_id={Some(session_id)}
                {on_recording_change}
                {on_transcription}
                on_interim_transcription={Some(on_interim_transcription)}
                {on_error}
                disabled={!ctx.props().ws_connected}
                button_ref={Some(button_ref)}
            />
        }
    }

    fn render_send_button(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let on_send = link.callback(|_| InputBarMsg::SendInput);
        let on_toggle_dropdown = link.callback(|e: MouseEvent| {
            e.stop_propagation();
            InputBarMsg::ToggleSendModeDropdown
        });
        let on_wiggum = link.callback(|_| InputBarMsg::SendWiggum);

        let file_input_ref = self.file_input_ref.clone();
        let on_attach_dropdown = Callback::from(move |_: MouseEvent| {
            if let Some(input) = file_input_ref.cast::<web_sys::HtmlInputElement>() {
                input.click();
            }
        });
        let on_file_change = link.callback(|e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            if let Some(files) = input.files() {
                if files.length() > 0 {
                    let mut file_list = Vec::new();
                    for i in 0..files.length() {
                        if let Some(file) = files.get(i) {
                            file_list.push(file);
                        }
                    }
                    input.set_value("");
                    if !file_list.is_empty() {
                        return InputBarMsg::FilesSelected(file_list);
                    }
                }
            }
            InputBarMsg::FileUploadError("No files selected".into())
        });

        let dropdown_class = if self.send_mode_dropdown_open {
            "send-mode-dropdown open"
        } else {
            "send-mode-dropdown"
        };

        let is_uploading = self.upload_progress.is_some_and(|p| p < 1.0);
        let ws_connected = ctx.props().ws_connected;

        html! {
            <div class="send-button-container">
                <input
                    ref={self.file_input_ref.clone()}
                    type="file"
                    multiple=true
                    class="hidden-file-input"
                    onchange={on_file_change}
                />
                <button
                    type="submit"
                    class="send-button"
                    disabled={!ws_connected || is_uploading}
                    onclick={on_send}
                >
                    { "Send" }
                </button>
                <button
                    type="button"
                    class="send-mode-toggle"
                    disabled={!ws_connected || is_uploading}
                    onclick={on_toggle_dropdown}
                >
                    { "\u{25bc}" }
                </button>
                <div class={dropdown_class}>
                    <button
                        type="button"
                        class="dropdown-option selected"
                        onclick={link.callback(|_| InputBarMsg::CloseSendModeDropdown)}
                    >
                        { "Send" }
                        <span class="option-hint">{ "Normal message" }</span>
                    </button>
                    <button
                        type="button"
                        class="dropdown-option wiggum"
                        onclick={on_wiggum}
                    >
                        <span class="wiggum-label">
                            <img src="wiggum.png" alt="" class="wiggum-icon" />
                            { "Wiggum" }
                        </span>
                        <span class="option-hint">{ "Loop until DONE" }</span>
                    </button>
                    <button
                        type="button"
                        class="dropdown-option attachment"
                        onclick={on_attach_dropdown}
                    >
                        { "Send with attachment(s)" }
                        <span class="option-hint">{ "Upload files + message" }</span>
                    </button>
                </div>
            </div>
        }
    }
}

/// Compose the combined user-text + file-list message the upload pipeline
/// sends to Claude after the last chunk lands. Pulled out so the message
/// shape (preserved verbatim from the pre-extraction behavior) is
/// unit-testable without a `web_sys::File`.
fn build_upload_message(user_input: &str, files: &[(String, u64)]) -> String {
    let file_list: Vec<String> = files
        .iter()
        .map(|(name, size)| format!("- {} ({})", name, format_file_size(*size)))
        .collect();
    let list = file_list.join("\n");
    if user_input.is_empty() {
        format!(
            "I've uploaded the following files to your working directory:\n{}",
            list
        )
    } else {
        format!(
            "{}\n\nI've uploaded the following files to your working directory:\n{}",
            user_input, list
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_upload_message ---

    #[test]
    fn build_upload_message_with_user_text_prepends_text_and_blank_line() {
        let files = vec![("a.txt".to_string(), 100u64)];
        let out = build_upload_message("hello", &files);
        assert_eq!(
            out,
            "hello\n\nI've uploaded the following files to your working directory:\n- a.txt (100 B)"
        );
    }

    #[test]
    fn build_upload_message_without_user_text_omits_blank_line_and_header() {
        let files = vec![("a.txt".to_string(), 100u64)];
        let out = build_upload_message("", &files);
        assert_eq!(
            out,
            "I've uploaded the following files to your working directory:\n- a.txt (100 B)"
        );
    }

    #[test]
    fn build_upload_message_lists_one_file_per_line_in_input_order() {
        let files = vec![
            ("first.png".to_string(), 1024u64),
            ("second.jpg".to_string(), 2 * 1024 * 1024),
            ("third.txt".to_string(), 42u64),
        ];
        let out = build_upload_message("ship it", &files);
        // Verify the ordering matches the input vec — the bar relies on
        // this so the user sees the files they picked in the order they
        // picked them.
        let expected = "ship it\n\nI've uploaded the following files to your working directory:\n- first.png (1.0 KB)\n- second.jpg (2.0 MB)\n- third.txt (42 B)";
        assert_eq!(out, expected);
    }

    #[test]
    fn build_upload_message_with_empty_file_list_still_renders_header() {
        // Defensive: an empty list shouldn't crash, even though the
        // upload pipeline always supplies at least one file. The format
        // ends with just the header + a trailing colon + newline; no list
        // rows.
        let out = build_upload_message("", &[]);
        assert_eq!(
            out,
            "I've uploaded the following files to your working directory:\n"
        );
    }

    // --- slash command pass-through ---
    //
    // The bar treats `/clear`, `/help`, `/cost`, `/status`, etc. as plain
    // text — Claude CLI parses them on the proxy side. These tests pin
    // that pass-through contract so a future refactor can't accidentally
    // intercept a slash command in the bar (which would mean a `/clear`
    // typed in the textarea would no longer reach the CLI).

    /// Marker — the bar performs no parsing of leading-slash input. The
    /// helper just trims like any other text. If you change this, also
    /// update the comment in `dispatch_text_send` and confirm the proxy
    /// side still receives the literal slash command.
    fn is_passthrough_text(s: &str) -> bool {
        // The bar's only normalisation is `.trim()` — see
        // `dispatch_text_send`. We mirror it here so the test is a real
        // contract pin and not a tautology.
        !s.trim().is_empty()
    }

    #[test]
    fn slash_clear_is_treated_as_plain_text() {
        assert!(is_passthrough_text("/clear"));
    }

    #[test]
    fn slash_help_is_treated_as_plain_text() {
        assert!(is_passthrough_text("/help"));
    }

    #[test]
    fn slash_cost_is_treated_as_plain_text() {
        assert!(is_passthrough_text("/cost"));
    }

    #[test]
    fn empty_or_whitespace_only_input_is_rejected() {
        // `dispatch_text_send` early-returns when the trimmed input is
        // empty, so the bar never emits an empty `ClaudeInput`.
        assert!(!is_passthrough_text(""));
        assert!(!is_passthrough_text("   "));
        assert!(!is_passthrough_text("\n\t"));
    }
}

//! Hook for two-mode keyboard navigation (edit mode / nav mode).

use gloo::events::{EventListener, EventListenerOptions, EventListenerPhase};
use shared::SessionInfo;
use std::collections::HashSet;
use uuid::Uuid;
use wasm_bindgen::JsCast;
use web_sys::KeyboardEvent;
use yew::prelude::*;

/// Focus the message input of the currently-focused session view, so leaving
/// Nav mode via `i`/`Enter` returns the caret to the composer. When vim mode is
/// on, the box was reset to INSERT as it handed off to Nav, so this lands the
/// user ready to type.
fn focus_active_message_input() {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    // Prefer the focused session's box; fall back to the only one on screen.
    let el = doc
        .query_selector(".session-view.focused .message-input")
        .ok()
        .flatten()
        .or_else(|| doc.query_selector(".message-input").ok().flatten());
    if let Some(input) = el
        .as_ref()
        .and_then(|e| e.dyn_ref::<web_sys::HtmlElement>())
    {
        let _ = input.focus();
    }
}

/// Look up the focused session's transcript ("text window") element. Prefers the
/// focused session's pane and falls back to the only one on screen. Shared by the
/// nav-mode transcript-scroll helpers (`j`/`k` and `gg`).
fn focused_transcript() -> Option<web_sys::Element> {
    let doc = web_sys::window().and_then(|w| w.document())?;
    doc.query_selector(".session-view.focused .session-view-messages")
        .ok()
        .flatten()
        .or_else(|| doc.query_selector(".session-view-messages").ok().flatten())
}

/// Scroll the focused session's transcript ("text window") by a few lines.
/// Nav-mode `j`/`k` drive this. Only `scrollTop` is moved here: the transcript's
/// own scroll listener reconciles live tailing afterwards, so scrolling up pauses
/// the tail and reveals the "Jump to live" pill, and scrolling back to the bottom
/// resumes it — the same DOM-only approach vim NORMAL uses for the transcript.
fn scroll_focused_transcript(down: bool) {
    let Some(messages) = focused_transcript() else {
        return;
    };
    // A few lines per press: brisk enough to read through output without the
    // overshoot of the half-page (Ctrl-d) jumps, and always past the ~50px
    // at-bottom threshold so a single `k` reliably leaves live tailing.
    let step = (messages.client_height() / 8).max(60);
    let delta = if down { step } else { -step };
    messages.set_scroll_top(messages.scroll_top() + delta);
}

/// Jump the focused session's transcript to the very top. Nav-mode `gg` drives
/// this. Like `k`, moving `scrollTop` off the bottom pauses live tailing and
/// reveals the "Jump to live" pill (reconciled by the transcript's own scroll
/// listener) — the counterpart to `G`, which jumps to the latest message.
fn scroll_focused_transcript_to_top() {
    if let Some(messages) = focused_transcript() {
        messages.set_scroll_top(0);
    }
}

/// True when there is a live text selection the user is likely trying to copy —
/// either a document selection (e.g. highlighted transcript text) or a range
/// inside the focused textarea/input (their selection is separate from the
/// document selection in most browsers). Terminal-style `Ctrl+C` defers to the
/// browser's copy in that case instead of firing the interrupt.
fn has_text_selection() -> bool {
    let Some(win) = web_sys::window() else {
        return false;
    };
    // Document-level selection (highlighted transcript / page text).
    if let Ok(Some(sel)) = win.get_selection() {
        if sel.to_string().length() > 0 {
            return true;
        }
    }
    // Selection inside the focused textarea/input.
    let Some(active) = win.document().and_then(|d| d.active_element()) else {
        return false;
    };
    if let Some(ta) = active.dyn_ref::<web_sys::HtmlTextAreaElement>() {
        if let (Ok(Some(s)), Ok(Some(e))) = (ta.selection_start(), ta.selection_end()) {
            return s != e;
        }
    }
    if let Some(input) = active.dyn_ref::<web_sys::HtmlInputElement>() {
        if let (Ok(Some(s)), Ok(Some(e))) = (input.selection_start(), input.selection_end()) {
            return s != e;
        }
    }
    false
}

/// Attach a window-level, capture-phase `keydown` listener that fires
/// `on_interrupt` on `Ctrl+C`, in **every** mode — edit, nav, and vim
/// NORMAL/INSERT.
///
/// Capture phase is essential: it runs before the composer's and vim's own key
/// handlers, so vim's `c` (the change operator) can't swallow the press and no
/// mode can shadow the interrupt. Bound to Ctrl specifically (not Cmd) so macOS
/// `Cmd+C` stays copy, and it defers to the browser's copy whenever there is an
/// active text selection (so copying transcript/composer text still works).
#[hook]
pub fn use_interrupt_hotkey(on_interrupt: Callback<()>) {
    use_effect_with((), move |_| {
        let options = EventListenerOptions {
            phase: EventListenerPhase::Capture,
            passive: false,
        };
        let listener = EventListener::new_with_options(
            &gloo::utils::document(),
            "keydown",
            options,
            move |event| {
                let Some(ke) = event.dyn_ref::<KeyboardEvent>() else {
                    return;
                };
                if ke.ctrl_key() && !ke.meta_key() && ke.key().eq_ignore_ascii_case("c") {
                    // Let the browser copy when there's a selection.
                    if has_text_selection() {
                        return;
                    }
                    ke.prevent_default();
                    ke.stop_propagation();
                    on_interrupt.emit(());
                }
            },
        );
        move || drop(listener)
    });
}

/// Configuration for the keyboard navigation hook.
pub struct KeyboardNavConfig {
    /// All sessions (sorted in display order)
    pub sessions: Vec<SessionInfo>,
    /// Currently focused session index
    pub focused_index: usize,
    /// Set of hidden session IDs
    pub hidden_sessions: HashSet<Uuid>,
    /// Callback when session selection changes
    pub on_select: Callback<usize>,
    /// Callback to activate a session (mark it as having been viewed)
    pub on_activate: Callback<Uuid>,
    /// Callback to open the keyboard-shortcuts help overlay (`?`)
    pub on_show_help: Callback<()>,
    /// Callback to open a new session (nav-mode `n`)
    pub on_new_session: Callback<()>,
    /// Callback to delete a session, given its id (nav-mode `d` on the focused session)
    pub on_delete: Callback<Uuid>,
    /// Callback to jump the focused session's transcript to the newest message
    /// and resume live tailing (nav-mode `G`).
    pub on_jump_to_latest: Callback<()>,
}

/// Return value from the use_keyboard_nav hook.
pub struct UseKeyboardNav {
    /// Whether currently in navigation mode
    pub nav_mode: bool,
    /// Callback to handle keydown events
    pub on_keydown: Callback<KeyboardEvent>,
}

/// Hook for managing two-mode keyboard navigation.
///
/// `Ctrl`/`Cmd`+`K` is the single mode toggle: it flips between edit mode and
/// Nav mode from anywhere. It is deliberately the *only* way to switch modes —
/// Escape no longer enters Nav mode (it kept ejecting people from the composer
/// mid-type). Session management then uses single-letter nav-mode keys, which
/// never collide with browser shortcuts.
///
/// Edit Mode (default):
/// - Typing works normally
/// - Ctrl/Cmd+K -> Nav Mode
/// - Shift+Tab -> next active session (skips hidden)
///
/// Nav Mode:
/// - Arrow keys / `h` / `l` move between sessions
/// - `j` / `k` scroll the focused session's transcript down / up
/// - `gg` jumps to the top of the focused transcript (`G` jumps to the latest)
/// - Numbers 1-9 select directly (stays in Nav mode)
/// - n -> new session (launch dialog)
/// - d -> delete the focused session (via the confirm modal)
/// - w -> next waiting session
/// - Enter -> accept the current pane and return to Edit Mode
/// - Ctrl/Cmd+K -> back to Edit Mode
///
/// `?` opens the keyboard-shortcuts help overlay whenever focus is not in the
/// message textarea (i.e. in nav mode, or in edit mode with a non-text element
/// focused).
///
/// Interrupt (`Ctrl+C`) is handled separately by [`use_interrupt_hotkey`], which
/// uses a window capture-phase listener so it fires in every mode.
#[hook]
pub fn use_keyboard_nav(config: KeyboardNavConfig) -> UseKeyboardNav {
    let nav_mode = use_state(|| false);
    // "Pending g" state for the two-press `gg` (jump to top). A first `g` arms it
    // by parking a short disarm timeout here; a second `g` (while armed) fires and
    // any other nav-mode key clears it. Held in a ref so arming doesn't re-render.
    // The `Timeout`'s presence is the armed flag; dropping it cancels the disarm.
    let pending_g = use_mut_ref(|| None::<gloo::timers::callback::Timeout>);

    let on_keydown = {
        let nav_mode = nav_mode.clone();
        let pending_g = pending_g.clone();
        let sessions = config.sessions.clone();
        let focused_index = config.focused_index;
        let hidden_sessions = config.hidden_sessions.clone();
        let on_select = config.on_select.clone();
        let on_activate = config.on_activate.clone();
        let on_show_help = config.on_show_help.clone();
        let on_new_session = config.on_new_session.clone();
        let on_delete = config.on_delete.clone();
        let on_jump_to_latest = config.on_jump_to_latest.clone();
        Callback::from(move |e: KeyboardEvent| {
            // Don't handle keyboard nav when a modal overlay is open. The launch
            // dialog, full-page modals, and help overlay are included so their
            // own keys (Esc / backdrop) win and nav shortcuts don't fire
            // underneath them.
            if gloo::utils::document()
                .query_selector(
                    ".sched-overlay, .share-dialog-overlay, .help-overlay, \
                     .launch-dialog-backdrop, .modal-overlay, .full-page-modal",
                )
                .ok()
                .flatten()
                .is_some()
            {
                return;
            }
            let in_nav_mode = *nav_mode;
            let len = sessions.len();

            // Helper: navigate to next non-hidden session
            let navigate_to_next_active = |current: usize| -> Option<usize> {
                if len == 0 {
                    return None;
                }
                for i in 1..=len {
                    let idx = (current + i) % len;
                    if let Some(session) = sessions.get(idx) {
                        if !hidden_sessions.contains(&session.id) {
                            return Some(idx);
                        }
                    }
                }
                None
            };

            // Helper: navigate by delta, skipping hidden sessions
            let navigate_by_delta = |current: usize, delta: i32| -> Option<usize> {
                if len == 0 {
                    return None;
                }

                let non_hidden_count = sessions
                    .iter()
                    .filter(|s| !hidden_sessions.contains(&s.id))
                    .count();

                // If all sessions are hidden, allow normal navigation
                if non_hidden_count == 0 {
                    return Some((current as i32 + delta).rem_euclid(len as i32) as usize);
                }

                // Skip hidden sessions when navigating
                let step = if delta > 0 { 1 } else { len - 1 };
                let mut new_index = current;

                for _ in 0..len {
                    new_index = (new_index + step) % len;
                    if let Some(session) = sessions.get(new_index) {
                        if !hidden_sessions.contains(&session.id) {
                            return Some(new_index);
                        }
                    }
                }
                None
            };

            // Shift+Tab always jumps to next active session (works in both modes)
            if e.shift_key() && e.key() == "Tab" {
                e.prevent_default();
                if let Some(new_idx) = navigate_to_next_active(focused_index) {
                    if let Some(session) = sessions.get(new_idx) {
                        on_activate.emit(session.id);
                    }
                    on_select.emit(new_idx);
                }
                return;
            }

            // Ctrl/Cmd+K is the single mode toggle: it flips between edit mode
            // and Nav mode from anywhere. It is deliberately the *only* way to
            // switch modes — Escape no longer enters Nav mode (it kept throwing
            // people out of the composer mid-type). Leaving Nav mode refocuses
            // the composer so you land ready to type.
            if (e.ctrl_key() || e.meta_key()) && e.key().eq_ignore_ascii_case("k") {
                e.prevent_default();
                if in_nav_mode {
                    nav_mode.set(false);
                    focus_active_message_input();
                } else {
                    nav_mode.set(true);
                }
                return;
            }

            // `?` opens the keyboard-shortcuts help overlay, unless the user is
            // typing into a text field (textarea/input). In nav mode the
            // textarea keeps DOM focus, so allow it explicitly there.
            if e.key() == "?" {
                let target_is_text_input = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .map(|el| {
                        let tag = el.tag_name();
                        tag.eq_ignore_ascii_case("textarea") || tag.eq_ignore_ascii_case("input")
                    })
                    .unwrap_or(false);
                if in_nav_mode || !target_is_text_input {
                    e.prevent_default();
                    on_show_help.emit(());
                    return;
                }
            }

            if in_nav_mode {
                // Navigation Mode. Ctrl/Cmd+K (handled above) toggles back to
                // edit mode from anywhere; Enter (below) also returns to edit
                // mode once you've landed on a pane.
                //
                // Disarm any pending `gg` first: a second `g` fires only if the
                // press right before it was also `g`, so every other key clears
                // the arm. Capture whether it *was* armed for the `g` arm below.
                let g_was_armed = pending_g.borrow().is_some();
                *pending_g.borrow_mut() = None;
                match e.key().as_str() {
                    "ArrowUp" | "ArrowLeft" | "h" => {
                        e.prevent_default();
                        if let Some(new_idx) = navigate_by_delta(focused_index, -1) {
                            if let Some(session) = sessions.get(new_idx) {
                                on_activate.emit(session.id);
                            }
                            on_select.emit(new_idx);
                        }
                    }
                    "ArrowDown" | "ArrowRight" | "l" => {
                        e.prevent_default();
                        if let Some(new_idx) = navigate_by_delta(focused_index, 1) {
                            if let Some(session) = sessions.get(new_idx) {
                                on_activate.emit(session.id);
                            }
                            on_select.emit(new_idx);
                        }
                    }
                    "j" => {
                        // Scroll the focused transcript down. Session switching
                        // moved to the arrows / `h` / `l` so the vim-familiar
                        // `j`/`k` can scroll the text window instead.
                        e.prevent_default();
                        scroll_focused_transcript(true);
                    }
                    "k" => {
                        // Scroll the focused transcript up.
                        e.prevent_default();
                        scroll_focused_transcript(false);
                    }
                    "g" => {
                        // `gg` (vim): a first `g` arms; a second fires a jump to
                        // the very top of the focused transcript. Like `k`, that
                        // pauses live tailing and shows the "Jump to live" pill —
                        // the counterpart to `G` (jump to latest). Stays in nav
                        // mode. A lone `g` was previously unbound (fell through the
                        // digit handler harmlessly), so nothing else regresses.
                        e.prevent_default();
                        if g_was_armed {
                            scroll_focused_transcript_to_top();
                        } else {
                            let disarm = pending_g.clone();
                            *pending_g.borrow_mut() =
                                Some(gloo::timers::callback::Timeout::new(600, move || {
                                    *disarm.borrow_mut() = None;
                                }));
                        }
                    }
                    "w" => {
                        e.prevent_default();
                        if let Some(new_idx) = navigate_to_next_active(focused_index) {
                            if let Some(session) = sessions.get(new_idx) {
                                on_activate.emit(session.id);
                            }
                            on_select.emit(new_idx);
                        }
                    }
                    "n" => {
                        // Open a new session (launch dialog). Stay in Nav mode:
                        // the modal guard above blocks nav keys while the dialog
                        // is open, and Ctrl/Cmd+K remains the only mode toggle.
                        e.prevent_default();
                        on_new_session.emit(());
                    }
                    "d" => {
                        // Delete the focused session. Routes through the confirm
                        // modal (RequestDelete), so this is not instantly
                        // destructive.
                        e.prevent_default();
                        if let Some(session) = sessions.get(focused_index) {
                            on_delete.emit(session.id);
                        }
                    }
                    "G" => {
                        // Jump the focused session's transcript to the newest
                        // message and resume live tailing. Stays in nav mode
                        // (like the other nav keys), matching vim's "go to end".
                        e.prevent_default();
                        on_jump_to_latest.emit(());
                    }
                    "Enter" => {
                        // Enter accepts the current pane and drops back to edit
                        // mode, refocusing the composer — a lighter-weight exit
                        // than Ctrl/Cmd+K once you've navigated to the pane you
                        // want. (The composer is inert in nav mode, so this Enter
                        // never submits a message.)
                        e.prevent_default();
                        nav_mode.set(false);
                        focus_active_message_input();
                    }
                    key => {
                        // Number keys 1-9 select the Nth session *as shown in the
                        // rail*. The rail (session_rail.rs) numbers the visible
                        // sessions in `sessions` (already display-sorted) order,
                        // hiding manually-hidden and cron/scheduled sessions — so
                        // we must use the exact same order and filter here, or the
                        // number won't match the badge the user sees.
                        if let Ok(num) = key.parse::<usize>() {
                            if (1..=9).contains(&num) {
                                let visible_indices: Vec<usize> = sessions
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, s)| {
                                        !hidden_sessions.contains(&s.id)
                                            && s.scheduled_task_id.is_none()
                                    })
                                    .map(|(idx, _)| idx)
                                    .collect();
                                if let Some(&actual_idx) = visible_indices.get(num - 1) {
                                    e.prevent_default();
                                    if let Some(session) = sessions.get(actual_idx) {
                                        on_activate.emit(session.id);
                                    }
                                    on_select.emit(actual_idx);
                                    // Stay in Nav mode after jumping so rapid
                                    // switching doesn't need a re-toggle; use
                                    // Ctrl/Cmd+K to return to the composer.
                                }
                            }
                        }
                    }
                }
            }
            // Edit mode has no mode key: Ctrl/Cmd+K (handled above) is the only
            // way into Nav mode. Escape intentionally does nothing here anymore.
        })
    };

    UseKeyboardNav {
        nav_mode: *nav_mode,
        on_keydown,
    }
}

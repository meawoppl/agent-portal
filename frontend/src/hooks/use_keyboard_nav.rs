//! Hook for two-mode keyboard navigation (edit mode / nav mode).

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
    /// Callback when triple-Escape interrupt is triggered
    pub on_interrupt: Callback<()>,
    /// Callback to open the keyboard-shortcuts help overlay (`?`)
    pub on_show_help: Callback<()>,
    /// Callback to open a new session (nav-mode `n`)
    pub on_new_session: Callback<()>,
    /// Callback to delete a session, given its id (nav-mode `d` on the focused session)
    pub on_delete: Callback<Uuid>,
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
/// - Arrow keys / hjkl navigate sessions
/// - Numbers 1-9 select directly (stays in Nav mode)
/// - n -> new session (launch dialog)
/// - d -> delete the focused session (via the confirm modal)
/// - w -> next waiting session
/// - Ctrl/Cmd+K -> back to Edit Mode
///
/// `?` opens the keyboard-shortcuts help overlay whenever focus is not in the
/// message textarea (i.e. in nav mode, or in edit mode with a non-text element
/// focused).
///
/// `Ctrl+C` interrupts the focused session (terminal-style). It defers to the
/// browser's copy when there is an active text selection.
#[hook]
pub fn use_keyboard_nav(config: KeyboardNavConfig) -> UseKeyboardNav {
    let nav_mode = use_state(|| false);

    let on_keydown = {
        let nav_mode = nav_mode.clone();
        let sessions = config.sessions.clone();
        let focused_index = config.focused_index;
        let hidden_sessions = config.hidden_sessions.clone();
        let on_select = config.on_select.clone();
        let on_activate = config.on_activate.clone();
        let on_interrupt = config.on_interrupt.clone();
        let on_show_help = config.on_show_help.clone();
        let on_new_session = config.on_new_session.clone();
        let on_delete = config.on_delete.clone();
        Callback::from(move |e: KeyboardEvent| {
            // Don't handle keyboard nav when a modal overlay is open. The help
            // overlay is included so its own keys (Esc / backdrop) win and nav
            // shortcuts don't fire underneath it.
            if gloo::utils::document()
                .query_selector(
                    ".sched-overlay, .share-dialog-overlay, .help-overlay, \
                     .launch-dialog-backdrop, .modal-overlay",
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

            // Ctrl+C interrupts the focused session (terminal-style). Bound to
            // Ctrl specifically (not Cmd) so macOS Cmd+C stays copy. When there
            // is an active text selection we defer to the browser's copy instead
            // of interrupting, so selecting transcript/composer text and copying
            // still works everywhere.
            if e.ctrl_key() && !e.meta_key() && e.key().eq_ignore_ascii_case("c") {
                if has_text_selection() {
                    return;
                }
                e.prevent_default();
                on_interrupt.emit(());
                return;
            }

            if in_nav_mode {
                // Navigation Mode. Ctrl/Cmd+K (handled above) is the only way
                // back to edit mode — no key here changes the mode.
                match e.key().as_str() {
                    "ArrowUp" | "ArrowLeft" | "k" | "h" => {
                        e.prevent_default();
                        if let Some(new_idx) = navigate_by_delta(focused_index, -1) {
                            if let Some(session) = sessions.get(new_idx) {
                                on_activate.emit(session.id);
                            }
                            on_select.emit(new_idx);
                        }
                    }
                    "ArrowDown" | "ArrowRight" | "j" | "l" => {
                        e.prevent_default();
                        if let Some(new_idx) = navigate_by_delta(focused_index, 1) {
                            if let Some(session) = sessions.get(new_idx) {
                                on_activate.emit(session.id);
                            }
                            on_select.emit(new_idx);
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

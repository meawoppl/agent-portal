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
}

/// Return value from the use_keyboard_nav hook.
pub struct UseKeyboardNav {
    /// Whether currently in navigation mode
    pub nav_mode: bool,
    /// Callback to handle keydown events
    pub on_keydown: Callback<KeyboardEvent>,
}

/// Max time window (ms) for 3 Escape presses to trigger an interrupt.
const TRIPLE_ESCAPE_WINDOW_MS: f64 = 600.0;

/// Hook for managing two-mode keyboard navigation.
///
/// Edit Mode (default):
/// - Typing works normally
/// - Escape -> Nav Mode
/// - Shift+Tab -> next active session (skips hidden)
///
/// Nav Mode:
/// - Arrow keys / hjkl navigate sessions
/// - Numbers 1-9 select directly
/// - Enter/Escape/i -> Edit Mode
/// - w -> next waiting session
///
/// Triple-Escape (within 600ms) sends an interrupt to the focused session.
#[hook]
pub fn use_keyboard_nav(config: KeyboardNavConfig) -> UseKeyboardNav {
    let nav_mode = use_state(|| false);
    // Track timestamps of recent Escape presses for triple-Escape detection
    let escape_times = use_mut_ref(Vec::<f64>::new);

    let on_keydown = {
        let nav_mode = nav_mode.clone();
        let escape_times = escape_times.clone();
        let sessions = config.sessions.clone();
        let focused_index = config.focused_index;
        let hidden_sessions = config.hidden_sessions.clone();
        let on_select = config.on_select.clone();
        let on_activate = config.on_activate.clone();
        let on_interrupt = config.on_interrupt.clone();
        Callback::from(move |e: KeyboardEvent| {
            // Don't handle keyboard nav when a modal overlay is open
            if gloo::utils::document()
                .query_selector(".sched-overlay, .share-dialog-overlay")
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

            // Track Escape presses for triple-Escape interrupt detection
            if e.key() == "Escape" {
                let now = js_sys::Date::now();
                let mut times = escape_times.borrow_mut();
                times.push(now);
                // Keep only presses within the time window
                times.retain(|&t| now - t <= TRIPLE_ESCAPE_WINDOW_MS);
                if times.len() >= 3 {
                    times.clear();
                    e.prevent_default();
                    // Return to edit mode and fire interrupt
                    nav_mode.set(false);
                    on_interrupt.emit(());
                    return;
                }
            }

            if in_nav_mode {
                // Navigation Mode
                match e.key().as_str() {
                    "Escape" | "i" => {
                        // Either key leaves Nav mode AND returns focus to the
                        // composer (ready to type). Esc is the natural "cycle
                        // back" key — INSERT →Esc→ NORMAL →Esc→ Nav →Esc→ typing —
                        // and refocusing on every exit means you can never get
                        // stranded in Nav mode with nothing focused.
                        e.prevent_default();
                        nav_mode.set(false);
                        focus_active_message_input();
                    }
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
                    "Enter" => {
                        e.prevent_default();
                        nav_mode.set(false);
                        focus_active_message_input();
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
                    "x" => {
                        // Placeholder for close session
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
                                    nav_mode.set(false);
                                }
                            }
                        }
                    }
                }
            } else {
                // Edit Mode
                if e.key().as_str() == "Escape" {
                    e.prevent_default();
                    nav_mode.set(true);
                }
            }
        })
    };

    UseKeyboardNav {
        nav_mode: *nav_mode,
        on_keydown,
    }
}

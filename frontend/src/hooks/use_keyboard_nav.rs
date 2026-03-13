//! Hook for two-mode keyboard navigation (edit mode / nav mode).

use shared::SessionInfo;
use std::collections::HashSet;
use uuid::Uuid;
use web_sys::KeyboardEvent;
use yew::prelude::*;

/// Configuration for the keyboard navigation hook.
pub struct KeyboardNavConfig {
    /// All sessions (sorted in display order)
    pub sessions: Vec<SessionInfo>,
    /// Currently focused session index
    pub focused_index: usize,
    /// Set of hidden session IDs
    pub hidden_sessions: HashSet<Uuid>,
    /// Set of connected session IDs
    pub connected_sessions: HashSet<Uuid>,
    /// Whether inactive sessions are hidden
    pub inactive_hidden: bool,
    /// Callback when session selection changes
    pub on_select: Callback<usize>,
    /// Callback to activate a session (mark it as having been viewed)
    pub on_activate: Callback<Uuid>,
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
/// # Arguments
/// * `config` - Configuration containing sessions, focused index, and callbacks
///
/// # Returns
/// * `UseKeyboardNav` - The current mode and keydown handler
///
#[hook]
pub fn use_keyboard_nav(config: KeyboardNavConfig) -> UseKeyboardNav {
    let nav_mode = use_state(|| false);

    let on_keydown = {
        let nav_mode = nav_mode.clone();
        let sessions = config.sessions.clone();
        let focused_index = config.focused_index;
        let hidden_sessions = config.hidden_sessions.clone();
        let connected_sessions = config.connected_sessions.clone();
        let inactive_hidden = config.inactive_hidden;
        let on_select = config.on_select.clone();
        let on_activate = config.on_activate.clone();
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

            if in_nav_mode {
                // Navigation Mode
                match e.key().as_str() {
                    "Escape" | "i" => {
                        e.prevent_default();
                        nav_mode.set(false);
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
                        // Number keys 1-9 for direct selection
                        if let Ok(num) = key.parse::<usize>() {
                            if (1..=9).contains(&num) {
                                // Build visible session indices in display order
                                let mut visible_indices: Vec<usize> = Vec::new();

                                // Add active sessions first
                                for (idx, session) in sessions.iter().enumerate() {
                                    let is_connected = connected_sessions.contains(&session.id);
                                    let is_hidden = hidden_sessions.contains(&session.id);
                                    if is_connected && !is_hidden {
                                        visible_indices.push(idx);
                                    }
                                }

                                // Add inactive sessions if not hidden
                                if !inactive_hidden {
                                    for (idx, session) in sessions.iter().enumerate() {
                                        let is_connected = connected_sessions.contains(&session.id);
                                        let is_hidden = hidden_sessions.contains(&session.id);
                                        if !is_connected || is_hidden {
                                            visible_indices.push(idx);
                                        }
                                    }
                                }

                                // Map display number (1-based) to actual index
                                let display_idx = num - 1;
                                if display_idx < visible_indices.len() {
                                    e.prevent_default();
                                    let actual_idx = visible_indices[display_idx];
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

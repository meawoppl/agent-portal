use super::page_state::{active_session_ids, DashboardSessionAction, DashboardSessionState};
use super::session_order;
use shared::SessionInfo;
use std::collections::HashSet;
use uuid::Uuid;
use yew::prelude::*;

pub(super) struct DashboardFocus {
    pub focused_index: usize,
    pub on_select_session: Callback<usize>,
    pub on_activate: Callback<Uuid>,
    pub on_interrupt: Callback<()>,
    pub interrupt_signal: u32,
}

#[hook]
pub(super) fn use_dashboard_focus(
    active_sessions: Vec<SessionInfo>,
    effective_hidden_sessions: HashSet<Uuid>,
    loading: bool,
    session_state: UseReducerHandle<DashboardSessionState>,
) -> DashboardFocus {
    // Derive the focused display index from the focused session id against the
    // current sorted order. Falls back to the first non-hidden session when the
    // focused id is absent (nothing focused yet, or the focused session was
    // deleted / left). The rail, keyboard nav, and focus render all consume
    // this derived index.
    let focused_index = session_order::resolve_focus_index(
        &active_sessions,
        session_state.focused_id,
        &effective_hidden_sessions,
    );

    // On initial load, focus first non-hidden session and activate all non-hidden sessions.
    {
        let active_sessions = active_sessions.clone();
        let effective_hidden_sessions = effective_hidden_sessions.clone();
        let session_state = session_state.clone();

        use_effect_with(
            (
                active_sessions.clone(),
                effective_hidden_sessions.clone(),
                loading,
            ),
            move |(sessions, hidden_sessions, is_loading)| {
                if !*is_loading && !sessions.is_empty() {
                    // Focus the first non-hidden session by id (falls through to
                    // the first session if all are hidden).
                    let first_focus = sessions
                        .iter()
                        .find(|s| !hidden_sessions.contains(&s.id))
                        .or_else(|| sessions.first())
                        .map(|s| s.id);

                    // Activate all non-hidden sessions so they load in background.
                    let activate_ids = sessions
                        .iter()
                        .filter(|s| !hidden_sessions.contains(&s.id))
                        .map(|s| s.id)
                        .collect();

                    session_state.dispatch(DashboardSessionAction::InitializeFocus {
                        focus_id: first_focus,
                        activate_ids,
                    });
                }
                || ()
            },
        );
    }

    // Auto-focus newly launched session when it appears in the session list.
    {
        let session_state = session_state.clone();

        use_effect_with(active_session_ids(&active_sessions), move |session_ids| {
            session_state.dispatch(DashboardSessionAction::FocusNewlyLaunched(
                session_ids.clone(),
            ));
            || ()
        });
    }

    let on_select_session = {
        let session_state = session_state.clone();
        let active_sessions = active_sessions.clone();
        // The rail / keyboard nav emit a display index valid against the order
        // that produced the current render; translate it to the session id so
        // focus stays attached to that session across later reorders.
        Callback::from(move |index: usize| {
            crate::audio::ensure_audio_context();
            crate::audio::play_sound(crate::audio::SoundEvent::SessionSwap);
            if let Some(session) = active_sessions.get(index) {
                session_state.dispatch(DashboardSessionAction::FocusAndActivate(session.id));
            }
        })
    };

    let on_activate = {
        let session_state = session_state.clone();
        Callback::from(move |session_id: Uuid| {
            session_state.dispatch(DashboardSessionAction::Activate(session_id));
        })
    };

    // Interrupt signal counter: incremented by triple-Escape, passed to focused SessionView.
    let interrupt_signal = use_state(|| 0u32);

    let on_interrupt = {
        let interrupt_signal = interrupt_signal.clone();
        Callback::from(move |()| {
            interrupt_signal.set(*interrupt_signal + 1);
        })
    };

    DashboardFocus {
        focused_index,
        on_select_session,
        on_activate,
        on_interrupt,
        interrupt_signal: *interrupt_signal,
    }
}

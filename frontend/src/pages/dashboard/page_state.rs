//! Pure reducer state for the dashboard's session-tracking UI.
//!
//! `DashboardPage` still owns side effects such as localStorage writes and
//! network calls. This reducer keeps the id/set bookkeeping in one tested
//! place so the page component can stay focused on orchestration.

use shared::SessionInfo;
use std::collections::HashSet;
use std::rc::Rc;
use uuid::Uuid;
use yew::Reducible;

use super::types::RailPosition;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DashboardSessionState {
    pub focused_id: Option<Uuid>,
    pub awaiting_sessions: HashSet<Uuid>,
    pub hidden_sessions: HashSet<Uuid>,
    pub connected_sessions: HashSet<Uuid>,
    pub activated_sessions: HashSet<Uuid>,
    pub initial_focus_set: bool,
    pub sessions_at_launch: Option<HashSet<Uuid>>,
}

impl DashboardSessionState {
    pub fn new(hidden_sessions: HashSet<Uuid>) -> Self {
        Self {
            focused_id: None,
            awaiting_sessions: HashSet::new(),
            hidden_sessions,
            connected_sessions: HashSet::new(),
            activated_sessions: HashSet::new(),
            initial_focus_set: false,
            sessions_at_launch: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DashboardSessionAction {
    InitializeFocus {
        focus_id: Option<Uuid>,
        activate_ids: Vec<Uuid>,
    },
    FocusAndActivate(Uuid),
    Activate(Uuid),
    SetAwaiting {
        session_id: Uuid,
        awaiting: bool,
    },
    SetConnected {
        session_id: Uuid,
        connected: bool,
    },
    SetHidden {
        session_id: Uuid,
        hidden: bool,
    },
    MessageSent(Uuid),
    FocusNewlyLaunched(Vec<Uuid>),
}

impl Reducible for DashboardSessionState {
    type Action = DashboardSessionAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut state = (*self).clone();

        match action {
            DashboardSessionAction::InitializeFocus {
                focus_id,
                activate_ids,
            } => {
                if state.initial_focus_set {
                    return self;
                }
                state.focused_id = focus_id;
                state.activated_sessions.extend(activate_ids);
                state.initial_focus_set = true;
            }
            DashboardSessionAction::FocusAndActivate(session_id) => {
                state.focused_id = Some(session_id);
                state.activated_sessions.insert(session_id);
            }
            DashboardSessionAction::Activate(session_id) => {
                state.activated_sessions.insert(session_id);
            }
            DashboardSessionAction::SetAwaiting {
                session_id,
                awaiting,
            } => {
                let changed = set_membership(&mut state.awaiting_sessions, session_id, awaiting);
                if !changed {
                    return self;
                }
            }
            DashboardSessionAction::SetConnected {
                session_id,
                connected,
            } => {
                let changed = set_membership(&mut state.connected_sessions, session_id, connected);
                if !changed {
                    return self;
                }
            }
            DashboardSessionAction::SetHidden { session_id, hidden } => {
                let changed = set_membership(&mut state.hidden_sessions, session_id, hidden);
                if !changed {
                    return self;
                }
            }
            DashboardSessionAction::MessageSent(session_id) => {
                if !state.awaiting_sessions.remove(&session_id) {
                    return self;
                }
            }
            DashboardSessionAction::FocusNewlyLaunched(session_ids) => {
                let Some(snapshot) = &state.sessions_at_launch else {
                    return self;
                };
                let Some(new_session_id) =
                    session_ids.into_iter().find(|id| !snapshot.contains(id))
                else {
                    return self;
                };

                state.focused_id = Some(new_session_id);
                state.activated_sessions.insert(new_session_id);
                state.sessions_at_launch = None;
            }
        }

        Rc::new(state)
    }
}

pub(super) fn active_session_ids(sessions: &[SessionInfo]) -> Vec<Uuid> {
    sessions.iter().map(|s| s.id).collect()
}

fn set_membership(set: &mut HashSet<Uuid>, id: Uuid, present: bool) -> bool {
    if present {
        set.insert(id)
    } else {
        set.remove(&id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DashboardUiState {
    pub show_launch_dialog: bool,
    pub show_admin: bool,
    pub show_settings: bool,
    pub show_help: bool,
    pub inactive_hidden: bool,
    pub show_cost: bool,
    pub rail_position: RailPosition,
    /// Opt-in: group the session rail's pills into per-host sections.
    pub group_by_host: bool,
    pub pending_leave: Option<Uuid>,
    pub pending_delete: Option<Uuid>,
}

impl DashboardUiState {
    pub fn new(
        inactive_hidden: bool,
        show_cost: bool,
        rail_position: RailPosition,
        group_by_host: bool,
    ) -> Self {
        Self {
            show_launch_dialog: false,
            show_admin: false,
            show_settings: false,
            show_help: false,
            inactive_hidden,
            show_cost,
            rail_position,
            group_by_host,
            pending_leave: None,
            pending_delete: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DashboardUiAction {
    ToggleLaunchDialog,
    CloseLaunchDialog,
    ShowAdmin,
    CloseAdmin,
    ShowSettings,
    CloseSettings,
    ShowHelp,
    CloseHelp,
    SetInactiveHidden(bool),
    SetShowCost(bool),
    SetRailPosition(RailPosition),
    SetGroupByHost(bool),
    RequestLeave(Uuid),
    ClearPendingLeave,
    RequestDelete(Uuid),
    ClearPendingDelete,
}

impl Reducible for DashboardUiState {
    type Action = DashboardUiAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut state = (*self).clone();

        match action {
            DashboardUiAction::ToggleLaunchDialog => {
                state.show_launch_dialog = !state.show_launch_dialog;
            }
            DashboardUiAction::CloseLaunchDialog => {
                state.show_launch_dialog = false;
            }
            DashboardUiAction::ShowAdmin => {
                state.show_admin = true;
            }
            DashboardUiAction::CloseAdmin => {
                state.show_admin = false;
            }
            DashboardUiAction::ShowSettings => {
                state.show_settings = true;
            }
            DashboardUiAction::CloseSettings => {
                state.show_settings = false;
            }
            DashboardUiAction::ShowHelp => {
                state.show_help = true;
            }
            DashboardUiAction::CloseHelp => {
                state.show_help = false;
            }
            DashboardUiAction::SetInactiveHidden(hidden) => {
                state.inactive_hidden = hidden;
            }
            DashboardUiAction::SetShowCost(show) => {
                state.show_cost = show;
            }
            DashboardUiAction::SetRailPosition(position) => {
                state.rail_position = position;
            }
            DashboardUiAction::SetGroupByHost(enabled) => {
                state.group_by_host = enabled;
            }
            DashboardUiAction::RequestLeave(session_id) => {
                state.pending_leave = Some(session_id);
            }
            DashboardUiAction::ClearPendingLeave => {
                state.pending_leave = None;
            }
            DashboardUiAction::RequestDelete(session_id) => {
                state.pending_delete = Some(session_id);
            }
            DashboardUiAction::ClearPendingDelete => {
                state.pending_delete = None;
            }
        }

        Rc::new(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn reduce(
        state: DashboardSessionState,
        action: DashboardSessionAction,
    ) -> Rc<DashboardSessionState> {
        Rc::new(state).reduce(action)
    }

    #[test]
    fn initialize_focus_runs_once_and_activates_visible_sessions() {
        let initial = DashboardSessionState::new(HashSet::new());
        let state = reduce(
            initial,
            DashboardSessionAction::InitializeFocus {
                focus_id: Some(id(1)),
                activate_ids: vec![id(1), id(2)],
            },
        );

        assert_eq!(state.focused_id, Some(id(1)));
        assert!(state.initial_focus_set);
        assert_eq!(state.activated_sessions, HashSet::from_iter([id(1), id(2)]));

        let state = state.reduce(DashboardSessionAction::InitializeFocus {
            focus_id: Some(id(3)),
            activate_ids: vec![id(3)],
        });

        assert_eq!(state.focused_id, Some(id(1)));
        assert_eq!(state.activated_sessions, HashSet::from_iter([id(1), id(2)]));
    }

    #[test]
    fn focus_and_activate_keeps_focus_by_session_id() {
        let state = reduce(
            DashboardSessionState::new(HashSet::new()),
            DashboardSessionAction::FocusAndActivate(id(42)),
        );

        assert_eq!(state.focused_id, Some(id(42)));
        assert!(state.activated_sessions.contains(&id(42)));
    }

    #[test]
    fn focus_newly_launched_uses_snapshot_delta() {
        let mut initial = DashboardSessionState::new(HashSet::new());
        initial.sessions_at_launch = Some(HashSet::from_iter([id(1), id(2)]));
        let state = Rc::new(initial);

        let state = state.reduce(DashboardSessionAction::FocusNewlyLaunched(vec![
            id(1),
            id(2),
            id(3),
        ]));

        assert_eq!(state.focused_id, Some(id(3)));
        assert!(state.activated_sessions.contains(&id(3)));
        assert_eq!(state.sessions_at_launch, None);
    }

    #[test]
    fn awaiting_connected_hidden_and_sent_actions_update_sets() {
        let state = DashboardSessionState::new(HashSet::new());
        let state = reduce(
            state,
            DashboardSessionAction::SetAwaiting {
                session_id: id(1),
                awaiting: true,
            },
        );
        let state = state.reduce(DashboardSessionAction::SetConnected {
            session_id: id(2),
            connected: true,
        });
        let state = state.reduce(DashboardSessionAction::SetHidden {
            session_id: id(3),
            hidden: true,
        });

        assert!(state.awaiting_sessions.contains(&id(1)));
        assert!(state.connected_sessions.contains(&id(2)));
        assert!(state.hidden_sessions.contains(&id(3)));

        let state = state.reduce(DashboardSessionAction::MessageSent(id(1)));
        assert!(!state.awaiting_sessions.contains(&id(1)));
    }

    fn reduce_ui(state: DashboardUiState, action: DashboardUiAction) -> Rc<DashboardUiState> {
        Rc::new(state).reduce(action)
    }

    #[test]
    fn ui_reducer_controls_modal_visibility() {
        let state = DashboardUiState::new(false, false, RailPosition::Top, false);
        let state = reduce_ui(state, DashboardUiAction::ToggleLaunchDialog);
        assert!(state.show_launch_dialog);

        let state = state.reduce(DashboardUiAction::CloseLaunchDialog);
        assert!(!state.show_launch_dialog);

        let state = state.reduce(DashboardUiAction::ShowAdmin);
        assert!(state.show_admin);
        let state = state.reduce(DashboardUiAction::CloseAdmin);
        assert!(!state.show_admin);

        let state = state.reduce(DashboardUiAction::ShowSettings);
        assert!(state.show_settings);
        let state = state.reduce(DashboardUiAction::CloseSettings);
        assert!(!state.show_settings);

        let state = state.reduce(DashboardUiAction::ShowHelp);
        assert!(state.show_help);
        let state = state.reduce(DashboardUiAction::CloseHelp);
        assert!(!state.show_help);
    }

    #[test]
    fn ui_reducer_tracks_preferences() {
        let state = DashboardUiState::new(false, false, RailPosition::Top, false);
        let state = reduce_ui(state, DashboardUiAction::SetInactiveHidden(true));
        let state = state.reduce(DashboardUiAction::SetShowCost(true));
        let state = state.reduce(DashboardUiAction::SetRailPosition(RailPosition::Left));
        let state = state.reduce(DashboardUiAction::SetGroupByHost(true));

        assert!(state.inactive_hidden);
        assert!(state.show_cost);
        assert_eq!(state.rail_position, RailPosition::Left);
        assert!(state.group_by_host);
    }

    #[test]
    fn ui_reducer_tracks_pending_confirmations() {
        let state = DashboardUiState::new(false, false, RailPosition::Top, false);
        let state = reduce_ui(state, DashboardUiAction::RequestLeave(id(1)));
        let state = state.reduce(DashboardUiAction::RequestDelete(id(2)));

        assert_eq!(state.pending_leave, Some(id(1)));
        assert_eq!(state.pending_delete, Some(id(2)));

        let state = state.reduce(DashboardUiAction::ClearPendingLeave);
        let state = state.reduce(DashboardUiAction::ClearPendingDelete);

        assert_eq!(state.pending_leave, None);
        assert_eq!(state.pending_delete, None);
    }
}

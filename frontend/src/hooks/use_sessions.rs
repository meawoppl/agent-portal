//! Fetch on user-channel invalidation, with an independent fallback per connection.
use crate::{
    federation::{Federation, FederationAction},
    pages::dashboard::PortalSession,
    portal_client::{use_portal, ConnId},
    utils::On401,
};
use shared::api::SessionsResponse;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

pub struct UseSessions {
    pub sessions: Vec<PortalSession>,
    pub loading: bool,
    /// The home server's last session poll failed. Its sessions stay listed
    /// as last seen until a poll succeeds.
    pub home_unreachable: bool,
    pub refresh: Callback<()>,
    pub set_sessions: Callback<Vec<PortalSession>>,
}

#[derive(Default, PartialEq)]
struct Lists {
    rows: HashMap<ConnId, Vec<PortalSession>>,
    loaded: HashSet<ConnId>,
    unreachable: HashSet<ConnId>,
}
enum ListAction {
    Received(ConnId, Vec<PortalSession>),
    /// A poll failed: keep the last rows rather than emptying the view.
    Unreachable(ConnId),
    Set(Vec<PortalSession>),
}
impl Reducible for Lists {
    type Action = ListAction;
    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        // An identical poll is a no-op: returning `self` skips rebuilding the
        // maps, and `use_reducer_eq` then sees equal state and skips the
        // render. Every five-second poll would otherwise re-render the
        // dashboard and every mounted session.
        match &action {
            ListAction::Received(conn, rows)
                if self.loaded.contains(conn)
                    && !self.unreachable.contains(conn)
                    && self.rows.get(conn) == Some(rows) =>
            {
                return self;
            }
            ListAction::Unreachable(conn) if self.unreachable.contains(conn) => return self,
            _ => {}
        }
        let mut next = Self {
            rows: self.rows.clone(),
            loaded: self.loaded.clone(),
            unreachable: self.unreachable.clone(),
        };
        match action {
            ListAction::Received(conn, rows) => {
                next.rows.insert(conn, rows);
                next.loaded.insert(conn);
                next.unreachable.remove(&conn);
            }
            ListAction::Unreachable(conn) => {
                next.unreachable.insert(conn);
            }
            ListAction::Set(rows) => {
                next.rows.clear();
                for row in rows {
                    next.rows.entry(row.key.conn).or_default().push(row);
                }
            }
        }
        next.into()
    }
}

/// Manual-refresh counter. A reducer rather than `use_state` so `refresh`
/// can be a stable callback: the increment happens here, not against a
/// value captured at some earlier render.
#[derive(Default, PartialEq)]
struct RefreshTick(u64);

impl Reducible for RefreshTick {
    type Action = ();
    fn reduce(self: Rc<Self>, (): ()) -> Rc<Self> {
        Rc::new(Self(self.0 + 1))
    }
}

#[hook]
pub fn use_sessions() -> UseSessions {
    let home = use_portal();
    let federation = use_context::<Federation>();
    let mut portals = vec![home];
    if let Some(registry) = &federation {
        portals.extend(registry.rows.iter().filter_map(|row| row.client.clone()));
    }
    // `_eq`: Yew 0.21's plain `use_reducer` re-renders on every dispatch,
    // even when `reduce` returns the same state.
    let lists = use_reducer_eq(Lists::default);
    let refresh_tick = use_reducer(RefreshTick::default);
    let revisions = super::use_client_websocket().session_list_revisions;
    let request_fetch = use_mut_ref(|| None::<Callback<Option<ConnId>>>);
    let seen_revisions = use_mut_ref(HashMap::new);
    {
        let lists = lists.clone();
        let federation = federation.clone();
        let request_fetch = request_fetch.clone();
        let seen_revisions = seen_revisions.clone();
        use_effect_with((portals.clone(), refresh_tick.0), move |(portals, _)| {
            let cancelled = Rc::new(Cell::new(false));
            let busy = Rc::new(RefCell::new(HashMap::new()));
            let fetch = {
                let portals = portals.clone();
                let cancelled = cancelled.clone();
                Callback::from(move |target: Option<ConnId>| {
                    for portal in portals
                        .iter()
                        .filter(|portal| target.is_none_or(|id| id == portal.conn()))
                    {
                        if busy.borrow_mut().insert(portal.conn(), false).is_some() {
                            busy.borrow_mut().insert(portal.conn(), true);
                            continue;
                        }
                        let (cancelled, busy, lists, federation) = (
                            cancelled.clone(),
                            busy.clone(),
                            lists.clone(),
                            federation.clone(),
                        );
                        let portal = portal.clone();
                        spawn_local(async move {
                            loop {
                                let result = portal
                                    .fetch_json::<SessionsResponse>("/api/sessions", On401::Logout)
                                    .await;
                                if cancelled.get() {
                                    return;
                                }
                                let (action, status) = match result {
                                    Ok(data) => (
                                        ListAction::Received(
                                            portal.conn(),
                                            data.sessions
                                                .into_iter()
                                                .map(|info| {
                                                    PortalSession::new(portal.clone(), info)
                                                })
                                                .collect(),
                                        ),
                                        "Connected".to_string(),
                                    ),
                                    Err(error) => (
                                        ListAction::Unreachable(portal.conn()),
                                        format!("Unavailable: {error}"),
                                    ),
                                };
                                if portal.is_linked() {
                                    if let Some(registry) = &federation {
                                        registry.dispatch(FederationAction::Status(
                                            portal.conn(),
                                            status,
                                        ));
                                    }
                                }
                                lists.dispatch(action);
                                if !busy.borrow_mut().remove(&portal.conn()).unwrap_or_default() {
                                    return;
                                }
                                busy.borrow_mut().insert(portal.conn(), false);
                            }
                        });
                    }
                })
            };
            fetch.emit(None);
            *request_fetch.borrow_mut() = Some(fetch.clone());
            let connections: Vec<_> = portals.iter().map(|portal| portal.conn()).collect();
            let mut ticks = 0;
            let timer = gloo::timers::callback::Interval::new(5_000, move || {
                ticks = (ticks + 1) % 12;
                if ticks == 0 {
                    fetch.emit(None);
                } else {
                    let seen = seen_revisions.borrow();
                    for &conn in &connections {
                        if !seen.contains_key(&conn) {
                            fetch.emit(Some(conn));
                        }
                    }
                }
            });
            move || {
                cancelled.set(true);
                request_fetch.borrow_mut().take();
                drop(timer);
            }
        });
    }
    {
        let request_fetch = request_fetch.clone();
        use_effect_with(revisions, move |revisions| {
            let previous = seen_revisions.replace(revisions.clone());
            if let Some(fetch) = request_fetch.borrow().as_ref() {
                for (conn, revision) in revisions {
                    if previous.get(conn) != Some(revision) {
                        fetch.emit(Some(*conn));
                    }
                }
            }
        });
    }
    // Stable callbacks: they are props of every mounted `SessionView`, and a
    // fresh `Callback` never compares equal.
    let refresh = {
        let tick = refresh_tick.dispatcher();
        use_callback((), move |_, ()| tick.dispatch(()))
    };
    let set_sessions = {
        let dispatcher = lists.dispatcher();
        use_callback((), move |rows, ()| {
            dispatcher.dispatch(ListAction::Set(rows))
        })
    };
    let current: HashSet<_> = portals.iter().map(|p| p.conn()).collect();
    UseSessions {
        // Removing/replacing a connection hides its old rows synchronously,
        // before any async fetch can complete.
        sessions: lists
            .rows
            .values()
            .flatten()
            .filter(|s| current.contains(&s.key.conn))
            .cloned()
            .collect(),
        loading: !lists.loaded.contains(&ConnId::HOME),
        home_unreachable: lists.unreachable.contains(&ConnId::HOME),
        refresh,
        set_sessions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed poll marks the connection unreachable but leaves its list as
    /// last seen; the next successful poll replaces it and clears the mark.
    #[test]
    fn a_failed_poll_keeps_the_last_list() {
        let home = ConnId::HOME;
        let state = Rc::new(Lists::default()).reduce(ListAction::Received(home, Vec::new()));
        let state = state.reduce(ListAction::Unreachable(home));
        assert!(state.unreachable.contains(&home));
        assert!(state.rows.contains_key(&home), "rows dropped on failure");
        assert!(state.loaded.contains(&home));
        let state = state.reduce(ListAction::Received(home, Vec::new()));
        assert!(!state.unreachable.contains(&home));
    }

    /// Before the first successful poll a failure leaves the list unloaded,
    /// so the page keeps waiting instead of showing the empty-state guide.
    #[test]
    fn a_failure_before_any_list_stays_unloaded() {
        let state = Rc::new(Lists::default()).reduce(ListAction::Unreachable(ConnId::HOME));
        assert!(!state.loaded.contains(&ConnId::HOME));
        assert!(state.unreachable.contains(&ConnId::HOME));
    }

    /// An identical poll leaves the state untouched, so `use_reducer_eq` skips
    /// the dashboard re-render; a changed one does not.
    #[test]
    fn an_unchanged_poll_keeps_the_same_state() {
        let home = ConnId::HOME;
        let state = Rc::new(Lists::default()).reduce(ListAction::Received(home, Vec::new()));
        let again = state.clone().reduce(ListAction::Received(home, Vec::new()));
        assert!(Rc::ptr_eq(&state, &again));

        let down = again.clone().reduce(ListAction::Unreachable(home));
        assert!(!Rc::ptr_eq(&again, &down));
        let still_down = down.clone().reduce(ListAction::Unreachable(home));
        assert!(Rc::ptr_eq(&down, &still_down));

        let recovered = still_down
            .clone()
            .reduce(ListAction::Received(home, Vec::new()));
        assert!(!Rc::ptr_eq(&still_down, &recovered));
    }
}

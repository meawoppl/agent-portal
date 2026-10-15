//! Poll each connection independently, discarding results after its lifecycle ends.
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
    pub loaded_connections: HashSet<ConnId>,
    pub refresh: Callback<()>,
    pub set_sessions: Callback<Vec<PortalSession>>,
}

#[derive(Default)]
struct Lists {
    rows: HashMap<ConnId, Vec<PortalSession>>,
    loaded: HashSet<ConnId>,
}
enum ListAction {
    Received(ConnId, Vec<PortalSession>),
    Set(Vec<PortalSession>),
}
impl Reducible for Lists {
    type Action = ListAction;
    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = Self {
            rows: self.rows.clone(),
            loaded: self.loaded.clone(),
        };
        match action {
            ListAction::Received(conn, rows) => {
                next.rows.insert(conn, rows);
                next.loaded.insert(conn);
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

#[hook]
pub fn use_sessions() -> UseSessions {
    let home = use_portal();
    let federation = use_context::<Federation>();
    let mut portals = vec![home];
    if let Some(registry) = &federation {
        portals.extend(registry.rows.iter().filter_map(|row| row.client.clone()));
    }
    let lists = use_reducer(Lists::default);
    let refresh_tick = use_state(|| 0u64);
    {
        let lists = lists.clone();
        let federation = federation.clone();
        use_effect_with((portals.clone(), *refresh_tick), move |(portals, _)| {
            let cancelled = Rc::new(Cell::new(false));
            let busy = Rc::new(RefCell::new(HashSet::new()));
            let fetch = {
                let portals = portals.clone();
                let cancelled = cancelled.clone();
                move || {
                    for portal in portals.clone() {
                        if !busy.borrow_mut().insert(portal.conn()) {
                            continue;
                        }
                        let (cancelled, busy, lists, federation) = (
                            cancelled.clone(),
                            busy.clone(),
                            lists.clone(),
                            federation.clone(),
                        );
                        spawn_local(async move {
                            let result = portal
                                .fetch_json::<SessionsResponse>("/api/sessions", On401::Logout)
                                .await;
                            if cancelled.get() {
                                return;
                            }
                            let (rows, status) = match result {
                                Ok(data) => (
                                    data.sessions
                                        .into_iter()
                                        .map(|info| PortalSession::new(portal.clone(), info))
                                        .collect(),
                                    "Connected".to_string(),
                                ),
                                Err(error) => (vec![], format!("Unavailable: {error}")),
                            };
                            if portal.is_linked() {
                                if let Some(registry) = &federation {
                                    registry
                                        .dispatch(FederationAction::Status(portal.conn(), status));
                                }
                            }
                            lists.dispatch(ListAction::Received(portal.conn(), rows));
                            busy.borrow_mut().remove(&portal.conn());
                        });
                    }
                }
            };
            fetch();
            let timer = gloo::timers::callback::Interval::new(5_000, fetch);
            move || {
                cancelled.set(true);
                drop(timer);
            }
        });
    }
    let refresh = {
        let tick = refresh_tick.clone();
        Callback::from(move |_| tick.set(*tick + 1))
    };
    let set_sessions = {
        let state = lists.clone();
        Callback::from(move |rows| state.dispatch(ListAction::Set(rows)))
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
        loaded_connections: lists.loaded.intersection(&current).copied().collect(),
        refresh,
        set_sessions,
    }
}

//! Account-scoped attention polling does not subscribe to background transcripts.
use super::PortalSession;
use crate::{
    portal_client::{ConnId, PortalClient, SessionKey},
    utils::On401,
};
use gloo::{events::EventListener, timers::callback::Interval};
use shared::api::{SessionAttention, SessionAttentionResponse, UpdateSessionAttention};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[derive(Clone, Default, PartialEq)]
struct AttentionState {
    rows: HashMap<SessionKey, SessionAttention>,
    loaded: HashSet<ConnId>,
    unavailable: HashSet<ConnId>,
    toast: Option<SessionKey>,
    mutation_failed: bool,
}
enum Action {
    Received(ConnId, Vec<SessionAttention>),
    Failed(ConnId),
    Dismiss,
    Updated(bool),
}
impl Reducible for AttentionState {
    type Action = Action;
    fn reduce(self: Rc<Self>, action: Action) -> Rc<Self> {
        let mut next = (*self).clone();
        match action {
            Action::Dismiss => next.toast = None,
            Action::Updated(success) => next.mutation_failed = !success,
            Action::Failed(conn) => {
                next.unavailable.insert(conn);
            }
            Action::Received(conn, rows) => {
                for row in &rows {
                    let key = SessionKey {
                        conn,
                        id: row.session_id,
                    };
                    if self.loaded.contains(&conn)
                        && row.ready()
                        && !row.muted
                        && self.rows.get(&key).is_none_or(|old| {
                            old.reply_id != row.reply_id
                                || (!old.needs_approval && row.needs_approval)
                        })
                    {
                        next.toast = Some(key);
                    }
                }
                next.rows.retain(|key, _| key.conn != conn);
                next.rows.extend(rows.into_iter().map(|row| {
                    (
                        SessionKey {
                            conn,
                            id: row.session_id,
                        },
                        row,
                    )
                }));
                next.loaded.insert(conn);
                next.unavailable.remove(&conn);
                if next
                    .toast
                    .is_some_and(|key| !next.rows.get(&key).is_some_and(SessionAttention::ready))
                {
                    next.toast = None;
                }
            }
        }
        next.into()
    }
}
pub(super) struct Attention {
    pub rows: HashMap<SessionKey, SessionAttention>,
    pub unavailable: bool,
    pub mutation_failed: bool,
    pub toast: Option<SessionKey>,
    pub dismiss: Callback<()>,
    pub mute: Callback<(SessionKey, bool)>,
    pub mark_read: Callback<(SessionKey, uuid::Uuid)>,
}

fn visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| !d.hidden() && d.has_focus().unwrap_or(false))
}
async fn update(portal: &PortalClient, key: SessionKey, request: &UpdateSessionAttention) -> bool {
    let Ok(request) = portal
        .post(&format!("/api/sessions/{}/attention", key.id))
        .json(request)
    else {
        return false;
    };
    request.send().await.is_ok_and(|response| response.ok())
}

#[hook]
pub(super) fn use_attention(
    portals: Vec<PortalClient>,
    sessions: Vec<PortalSession>,
    focused: Option<SessionKey>,
) -> Attention {
    let state = use_reducer_eq(AttentionState::default);
    let client_id = use_state(uuid::Uuid::new_v4);
    // Mutable snapshots keep long-lived timers from reading mount-time props.
    let latest = use_mut_ref(|| (sessions.clone(), focused));
    *latest.borrow_mut() = (sessions.clone(), focused);
    {
        let state = state.clone();
        let latest = latest.clone();
        let client_id = *client_id;
        use_effect_with(portals.clone(), move |portals| {
            let cancelled = Rc::new(Cell::new(false));
            let busy = Rc::new(RefCell::new(HashSet::new()));
            let previous_view = Rc::new(RefCell::new(None::<(PortalClient, SessionKey)>));
            let refresh = Rc::new({
                let portals = portals.clone();
                let cancelled = cancelled.clone();
                move || {
                    if cancelled.get() {
                        return;
                    }
                    let focused = latest.borrow().1;
                    let session = latest
                        .borrow()
                        .0
                        .iter()
                        .find(|s| Some(s.key) == focused)
                        .cloned();
                    let doc = web_sys::window().and_then(|w| w.document());
                    let element = doc.as_ref().and_then(|d| {
                        d.query_selector(".session-view-wrapper.focused .session-view-messages")
                            .ok()
                            .flatten()
                    });
                    let viewing = visible()
                        && !crate::hooks::modal_overlay_open()
                        && element.as_ref().is_some_and(|e| {
                            e.client_height() > 0
                                && e.scroll_height() - e.scroll_top() - e.client_height() < 50
                        });
                    let viewed = session.filter(|s| viewing && s.portal.can_write());
                    let new_key = viewed.as_ref().map(|s| s.key);
                    let old = previous_view.borrow().clone();
                    if let Some((portal, key)) = old.filter(|(_, key)| Some(*key) != new_key) {
                        let cancelled = cancelled.clone();
                        spawn_local(async move {
                            if cancelled.get() {
                                return;
                            }
                            update(
                                &portal,
                                key,
                                &UpdateSessionAttention {
                                    client_id,
                                    viewing: false,
                                    seen_reply_id: None,
                                    muted: None,
                                },
                            )
                            .await;
                        });
                        *previous_view.borrow_mut() = None;
                    }
                    if let Some(session) = viewed {
                        *previous_view.borrow_mut() = Some((session.portal.clone(), session.key));
                        let seen_reply_id = element
                            .and_then(|e| e.get_attribute("data-rendered-reply"))
                            .and_then(|id| id.parse().ok());
                        let cancelled = cancelled.clone();
                        spawn_local(async move {
                            if cancelled.get() {
                                return;
                            }
                            update(
                                &session.portal,
                                session.key,
                                &UpdateSessionAttention {
                                    client_id,
                                    viewing: true,
                                    seen_reply_id,
                                    muted: None,
                                },
                            )
                            .await;
                        });
                    }
                    if !visible() {
                        return;
                    }
                    for portal in portals.clone() {
                        if !busy.borrow_mut().insert(portal.conn()) {
                            continue;
                        }
                        let (state, busy, cancelled) =
                            (state.clone(), busy.clone(), cancelled.clone());
                        spawn_local(async move {
                            if cancelled.get() {
                                busy.borrow_mut().remove(&portal.conn());
                                return;
                            }
                            let result = portal
                                .fetch_json::<SessionAttentionResponse>(
                                    "/api/session-attention",
                                    On401::Ignore,
                                )
                                .await;
                            busy.borrow_mut().remove(&portal.conn());
                            if cancelled.get() {
                                return;
                            }
                            state.dispatch(match result {
                                Ok(data) => Action::Received(portal.conn(), data.sessions),
                                Err(_) => Action::Failed(portal.conn()),
                            });
                        });
                    }
                }
            });
            refresh();
            let timer = Interval::new(3_000, {
                let refresh = refresh.clone();
                move || refresh()
            });
            let visibility = web_sys::window().and_then(|w| w.document()).map(|d| {
                EventListener::new(&d, "visibilitychange", {
                    let refresh = refresh.clone();
                    move |_| refresh()
                })
            });
            let focus = web_sys::window().map(|w| {
                EventListener::new(&w, "focus", {
                    let refresh = refresh.clone();
                    move |_| refresh()
                })
            });
            let blur = web_sys::window().map(|w| {
                EventListener::new(&w, "blur", {
                    let refresh = refresh.clone();
                    move |_| refresh()
                })
            });
            // WebKit can throw synchronously if a queued fetch starts after
            // pagehide. Stop queued work too; leases expire after a closed tab.
            let pagehide = web_sys::window().map(|w| {
                EventListener::new(&w, "pagehide", {
                    let cancelled = cancelled.clone();
                    move |_| cancelled.set(true)
                })
            });
            let pageshow = web_sys::window().map(|w| {
                EventListener::new(&w, "pageshow", {
                    let cancelled = cancelled.clone();
                    move |_| {
                        cancelled.set(false);
                        refresh();
                    }
                })
            });
            move || {
                cancelled.set(true);
                drop((timer, visibility, focus, blur, pagehide, pageshow));
            }
        });
    }
    let mutate = {
        let state = state.clone();
        let sessions = sessions.clone();
        Callback::from(
            move |(key, request): (SessionKey, UpdateSessionAttention)| {
                if let Some(s) = sessions
                    .iter()
                    .find(|s| s.key == key && s.portal.can_write())
                    .cloned()
                {
                    let state = state.clone();
                    spawn_local(async move {
                        let success = update(&s.portal, key, &request).await;
                        state.dispatch(Action::Updated(success));
                    });
                }
            },
        )
    };
    use_effect_with(state.toast, |toast| {
        if toast.is_some() {
            crate::notification_sound::play();
        }
        || ()
    });
    let current: HashSet<_> = portals.iter().map(PortalClient::conn).collect();
    Attention {
        rows: state
            .rows
            .iter()
            .filter(|(k, _)| current.contains(&k.conn))
            .map(|(k, v)| (*k, v.clone()))
            .collect(),
        unavailable: current.iter().any(|c| state.unavailable.contains(c)),
        mutation_failed: state.mutation_failed,
        toast: state.toast.filter(|k| current.contains(&k.conn)),
        dismiss: {
            let state = state.clone();
            Callback::from(move |_| state.dispatch(Action::Dismiss))
        },
        mute: {
            let client_id = *client_id;
            mutate.reform(move |(key, muted)| {
                (
                    key,
                    UpdateSessionAttention {
                        client_id,
                        viewing: false,
                        seen_reply_id: None,
                        muted: Some(muted),
                    },
                )
            })
        },
        mark_read: {
            let client_id = *client_id;
            // Explicit acknowledgments also work after transcript retention. Keep
            // the displayed reply ID so a delayed click cannot consume a newer one.
            mutate.reform(move |(key, reply_id)| {
                (
                    key,
                    UpdateSessionAttention {
                        client_id,
                        viewing: false,
                        seen_reply_id: Some(reply_id),
                        muted: None,
                    },
                )
            })
        },
    }
}

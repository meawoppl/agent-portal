//! The session ⋯ menu and the dialogs it opens, owned in one place so the
//! desktop rail and the phone Sessions sheet offer the same actions with the
//! same confirmations. The phone layout has no rail, so without this the
//! sheet had no way to stop, pause, leave or close a session.
//!
//! Callers render `SessionMenu::view` where the menu should stack: the rail
//! beside its pills, the sheet inside itself, so the menu and its dialogs sit
//! above the sheet and inside its focus trap.

use super::menu::SessionRailMenu;
use super::{clamped_pill_menu_position, PortalSession};
use crate::components::{ForkDialog, RenameSessionDialog, ScheduleDialog, ShareDialog};
use crate::portal_client::{PortalClient, SessionKey};
use gloo::events::EventListener;
use std::collections::HashSet;
use web_sys::{HtmlElement, MouseEvent};
use yew::prelude::*;

/// What the menu acts on and how its actions reach the dashboard.
pub(crate) struct SessionMenuInputs {
    pub sessions: Vec<PortalSession>,
    pub hidden: HashSet<SessionKey>,
    pub connected: HashSet<SessionKey>,
    /// With archiving off, closing a session is destructive; the armed close
    /// hint says so.
    pub archive_enabled: bool,
    pub on_stop: Callback<SessionKey>,
    pub on_toggle_hidden: Callback<SessionKey>,
    pub on_toggle_pause: Callback<(SessionKey, bool)>,
    pub on_leave: Callback<SessionKey>,
    pub on_delete: Callback<SessionKey>,
    pub on_renamed: Callback<()>,
}

pub(crate) struct SessionMenu {
    /// Wire to a session's ⋯ button: opens its menu under the button, or
    /// closes it when it is already open.
    pub toggle: Callback<(SessionKey, MouseEvent)>,
    /// Close the menu. For containers that stop click propagation (the
    /// phone sheet), so clicks inside them never reach the document listener.
    pub close: Callback<()>,
    /// Whether a session's menu is open.
    pub is_open: bool,
    /// The open dropdown and any dialog it started.
    pub view: Html,
}

/// Render a menu dialog against the portal its session lives on, so every
/// request it makes reaches that portal.
fn on_session_portal(portal: &PortalClient, dialog: Html) -> Html {
    html! {
        <ContextProvider<PortalClient> context={portal.clone()}>
            { dialog }
        </ContextProvider<PortalClient>>
    }
}

#[hook]
pub(crate) fn use_session_menu(inputs: SessionMenuInputs) -> SessionMenu {
    let menu_session = use_state(|| None::<SessionKey>);
    let menu_pos = use_state(|| (0i32, 0i32));
    let stop_confirm = use_state(|| false);
    let delete_confirm = use_state(|| false);
    let copied_id = use_state(|| false);
    let share_session = use_state(|| None::<PortalSession>);
    let schedule_session = use_state(|| None::<PortalSession>);
    let fork_session = use_state(|| None::<PortalSession>);
    let rename_session = use_state(|| None::<PortalSession>);

    let reset = {
        let menu_session = menu_session.clone();
        let stop_confirm = stop_confirm.clone();
        let delete_confirm = delete_confirm.clone();
        std::rc::Rc::new(move || {
            menu_session.set(None);
            stop_confirm.set(false);
            delete_confirm.set(false);
        })
    };

    // Any click that reaches the document is outside the menu: the dropdown
    // and the ⋯ buttons stop their own clicks from propagating.
    {
        let reset = reset.clone();
        let is_open = (*menu_session).is_some();
        use_effect_with(is_open, move |is_open| {
            let listener = is_open
                .then(|| EventListener::new(&gloo::utils::document(), "click", move |_| reset()));
            move || drop(listener)
        });
    }

    let toggle = {
        let menu_session = menu_session.clone();
        let menu_pos = menu_pos.clone();
        let stop_confirm = stop_confirm.clone();
        let delete_confirm = delete_confirm.clone();
        let copied_id = copied_id.clone();
        Callback::from(move |(session_key, e): (SessionKey, MouseEvent)| {
            e.stop_propagation();
            stop_confirm.set(false);
            delete_confirm.set(false);
            copied_id.set(false);
            if *menu_session == Some(session_key) {
                menu_session.set(None);
                return;
            }
            if let Some(el) = e.target_dyn_into::<HtmlElement>() {
                let rect = el.get_bounding_client_rect();
                let (viewport_width, viewport_height) = web_sys::window()
                    .map(|window| {
                        let dimension = |value: Result<wasm_bindgen::JsValue, _>, fallback| {
                            value.ok().and_then(|v| v.as_f64()).unwrap_or(fallback) as i32
                        };
                        (
                            dimension(window.inner_width(), 800.0),
                            dimension(window.inner_height(), 600.0),
                        )
                    })
                    .unwrap_or((800, 600));
                menu_pos.set(clamped_pill_menu_position(
                    rect.left() as i32,
                    rect.bottom() as i32,
                    viewport_width,
                    viewport_height,
                ));
            }
            menu_session.set(Some(session_key));
        })
    };

    let open_session: Option<PortalSession> = (*menu_session)
        .and_then(|key| inputs.sessions.iter().find(|s| s.key == key))
        .cloned();
    let open_key = open_session.as_ref().map(|session| session.key);
    let opener = |state: &UseStateHandle<Option<PortalSession>>| {
        let state = state.clone();
        Callback::from(move |session| state.set(Some(session)))
    };
    let dialog = |state: &UseStateHandle<Option<PortalSession>>,
                  render: &dyn Fn(&PortalSession, Callback<()>) -> Html| {
        state.as_ref().map_or_else(Html::default, |session| {
            let state = state.clone();
            let on_close = Callback::from(move |_| state.set(None));
            on_session_portal(&session.portal, render(session, on_close))
        })
    };
    let on_renamed = inputs.on_renamed.clone();

    let view = html! {
        <>
            <SessionRailMenu
                session={open_session}
                position={*menu_pos}
                is_hidden={open_key.is_some_and(|key| inputs.hidden.contains(&key))}
                is_connected={open_key.is_some_and(|key| inputs.connected.contains(&key))}
                confirming_stop={*stop_confirm}
                confirming_delete={*delete_confirm}
                archive_enabled={inputs.archive_enabled}
                copied_id={*copied_id}
                on_close={{ let menu_session = menu_session.clone(); Callback::from(move |()| menu_session.set(None)) }}
                on_set_stop_confirm={{ let s = stop_confirm.clone(); Callback::from(move |v| s.set(v)) }}
                on_set_delete_confirm={{ let s = delete_confirm.clone(); Callback::from(move |v| s.set(v)) }}
                on_set_copied_id={{ let s = copied_id.clone(); Callback::from(move |v| s.set(v)) }}
                on_stop={inputs.on_stop}
                on_toggle_hidden={inputs.on_toggle_hidden}
                on_toggle_pause={inputs.on_toggle_pause}
                on_leave={inputs.on_leave}
                on_delete={inputs.on_delete}
                on_share={opener(&share_session)}
                on_schedule={opener(&schedule_session)}
                on_fork={opener(&fork_session)}
                on_rename={opener(&rename_session)}
            />
            { dialog(&rename_session, &|session, on_close| html! {
                <RenameSessionDialog key={session.key.to_string()} session_id={session.info.id}
                    current_name={session.info.session_name.clone()} {on_close} on_renamed={on_renamed.clone()} />
            }) }
            { dialog(&share_session, &|session, on_close| html! {
                <ShareDialog session_id={session.info.id} {on_close} />
            }) }
            { dialog(&schedule_session, &|session, on_close| html! {
                <ScheduleDialog session={Some(session.info.clone())} {on_close} />
            }) }
            { dialog(&fork_session, &|session, on_close| html! {
                <ForkDialog session={session.info.clone()} {on_close} />
            }) }
        </>
    };

    let close = Callback::from(move |()| reset());
    SessionMenu {
        toggle,
        close,
        is_open: (*menu_session).is_some(),
        view,
    }
}

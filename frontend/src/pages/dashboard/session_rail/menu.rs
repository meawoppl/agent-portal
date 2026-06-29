use shared::SessionInfo;
use uuid::Uuid;
use wasm_bindgen_futures::JsFuture;
use web_sys::MouseEvent;
use yew::prelude::*;

use super::{repo_pr_menu_hint, sorted_prs};

#[derive(Properties, PartialEq)]
pub(super) struct SessionRailMenuProps {
    pub session: Option<SessionInfo>,
    pub position: (i32, i32),
    pub is_hidden: bool,
    pub is_connected: bool,
    pub stop_has_tasks: bool,
    pub confirming_stop: bool,
    pub copied_id: bool,
    pub on_close: Callback<()>,
    pub on_set_stop_confirm: Callback<bool>,
    pub on_set_copied_id: Callback<bool>,
    pub on_stop: Callback<Uuid>,
    pub on_toggle_hidden: Callback<Uuid>,
    pub on_toggle_pause: Callback<(Uuid, bool)>,
    pub on_leave: Callback<Uuid>,
    pub on_delete: Callback<Uuid>,
    pub on_share: Callback<Uuid>,
    pub on_schedule: Callback<SessionInfo>,
}

#[function_component(SessionRailMenu)]
pub(super) fn session_rail_menu(props: &SessionRailMenuProps) -> Html {
    let is_menu_open = props.session.is_some();
    let dropdown_class = if is_menu_open {
        "pill-dropdown open"
    } else {
        "pill-dropdown"
    };

    let (left, top) = props.position;
    let dropdown_style = if is_menu_open {
        format!("left: {left}px; top: {top}px;")
    } else {
        String::new()
    };

    html! {
        <div
            class={dropdown_class}
            style={dropdown_style}
            onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
        >
            {
                if let Some(session) = &props.session {
                    render_menu_content(session, props)
                } else {
                    html! {}
                }
            }
        </div>
    }
}

fn render_menu_content(session: &SessionInfo, props: &SessionRailMenuProps) -> Html {
    let is_hidden = props.is_hidden;
    let is_connected = props.is_connected;
    let is_paused = session.paused;
    let session_id = session.id;

    let on_stop = {
        let on_stop = props.on_stop.clone();
        let on_close = props.on_close.clone();
        let on_set_stop_confirm = props.on_set_stop_confirm.clone();
        let confirming_stop = props.confirming_stop;
        Callback::from(move |_: MouseEvent| {
            if confirming_stop {
                on_stop.emit(session_id);
                on_set_stop_confirm.emit(false);
                on_close.emit(());
            } else {
                on_set_stop_confirm.emit(true);
            }
        })
    };

    let open_schedule = close_then(props.on_close.clone(), {
        let on_schedule = props.on_schedule.clone();
        let session = session.clone();
        move || on_schedule.emit(session.clone())
    });

    let on_hide = close_then(props.on_close.clone(), {
        let on_toggle_hidden = props.on_toggle_hidden.clone();
        move || on_toggle_hidden.emit(session_id)
    });

    let on_toggle_pause = close_then(props.on_close.clone(), {
        let on_toggle_pause = props.on_toggle_pause.clone();
        move || on_toggle_pause.emit((session_id, !is_paused))
    });

    let on_leave = close_then(props.on_close.clone(), {
        let on_leave = props.on_leave.clone();
        move || on_leave.emit(session_id)
    });

    let on_delete = close_then(props.on_close.clone(), {
        let on_delete = props.on_delete.clone();
        move || on_delete.emit(session_id)
    });

    let hide_label = if is_hidden {
        "Show Session"
    } else {
        "Hide Session"
    };
    let hide_hint = if is_hidden {
        "Show in rotation"
    } else {
        "Hide from rotation"
    };

    let hide_option = if is_paused {
        html! {
            <span class="pill-menu-option disabled">
                { "Hidden While Paused" }
                <span class="option-hint">{ "Resume to show in rotation" }</span>
            </span>
        }
    } else {
        menu_option(
            classes!("hide", is_hidden.then_some("active")),
            hide_label,
            hide_hint,
            on_hide,
        )
    };

    let pause_option = if session.my_role != "viewer" {
        let (pause_label, pause_hint) = if is_paused {
            ("Resume Session", "Restart from saved session")
        } else {
            ("Pause Session", "Stop and suppress auto-resume")
        };
        menu_option(
            classes!("pause", is_paused.then_some("active")),
            pause_label,
            pause_hint,
            on_toggle_pause,
        )
    } else {
        html! {}
    };

    let stop_option =
        if is_paused || (is_connected && session.status == shared::SessionStatus::Active) {
            if props.stop_has_tasks {
                menu_option(
                    classes!("stop", "blocked"),
                    "Delete Scheduled Tasks First",
                    "Opens task manager",
                    open_schedule.clone(),
                )
            } else {
                let (stop_label, stop_hint) = if props.confirming_stop {
                    let hint = if is_paused {
                        "This will remove the saved launcher entry"
                    } else {
                        "This will terminate the process"
                    };
                    ("Click again to confirm", hint)
                } else if is_paused {
                    ("Stop Session", "Remove saved launcher entry")
                } else {
                    ("Stop Session", "Terminate process")
                };
                menu_option(
                    classes!("stop", props.confirming_stop.then_some("confirming")),
                    stop_label,
                    stop_hint,
                    on_stop,
                )
            }
        } else {
            html! {}
        };

    let on_copy_id = {
        let on_set_copied_id = props.on_set_copied_id.clone();
        Callback::from(move |_: MouseEvent| {
            let window = web_sys::window().expect("no window");
            let clipboard = window.navigator().clipboard();
            let id_str = session_id.to_string();
            let on_set_copied_id = on_set_copied_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = JsFuture::from(clipboard.write_text(&id_str)).await;
                on_set_copied_id.emit(true);
                let on_set_copied_id = on_set_copied_id.clone();
                gloo::timers::callback::Timeout::new(1_500, move || {
                    on_set_copied_id.emit(false);
                })
                .forget();
            });
        })
    };
    let copy_label = if props.copied_id {
        "Copied!"
    } else {
        "Session ID"
    };
    let short_id = &session.id.to_string()[..8];

    let leave_option = if session.my_role != "owner" {
        menu_option(
            classes!("leave"),
            "Leave Session",
            "Remove from your list",
            on_leave,
        )
    } else {
        html! {}
    };

    let repo_option = repo_pr_submenu(session);

    let share_option = if session.my_role == "owner" {
        let on_share = close_then(props.on_close.clone(), {
            let on_share = props.on_share.clone();
            move || on_share.emit(session_id)
        });
        menu_option(
            classes!("share"),
            "Share Session",
            "Manage access",
            on_share,
        )
    } else {
        html! {}
    };

    let delete_option = if session.my_role == "owner" {
        menu_option(
            classes!("stop"),
            "Delete Session",
            "Remove history and metadata",
            on_delete,
        )
    } else {
        html! {}
    };

    let schedule_option = if session.my_role == "owner" {
        menu_option(
            classes!("schedule"),
            "Schedule Task",
            "Cron jobs",
            open_schedule,
        )
    } else {
        html! {}
    };

    html! {
        <>
            { menu_option(
                classes!("copy-id", props.copied_id.then_some("copied")),
                copy_label,
                short_id,
                on_copy_id,
            ) }
            { share_option }
            { schedule_option }
            { repo_option }
            { pause_option }
            { hide_option }
            { leave_option }
            { stop_option }
            { delete_option }
        </>
    }
}

/// Build a dropdown click handler that runs `action` and then closes the menu.
fn close_then(on_close: Callback<()>, action: impl Fn() + 'static) -> Callback<MouseEvent> {
    Callback::from(move |_: MouseEvent| {
        action();
        on_close.emit(());
    })
}

/// Render one dropdown menu option button with the shared pill-menu shell.
fn menu_option(extra: Classes, label: &str, hint: &str, onclick: Callback<MouseEvent>) -> Html {
    html! {
        <button type="button" class={classes!("pill-menu-option", extra)} {onclick}>
            { label }
            <span class="option-hint">{ hint }</span>
        </button>
    }
}

fn repo_pr_submenu(session: &SessionInfo) -> Html {
    let prs = sorted_prs(&session.open_prs);
    if session.repo_url.is_none() && prs.is_empty() {
        return html! {
            <span class="pill-menu-option disabled">
                { "No Repository Detected" }
            </span>
        };
    }

    let repo_link = if let Some(ref url) = session.repo_url {
        let href = url.clone();
        html! {
            <a class="pill-menu-option pr-link" href={href} target="_blank"
               onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                { "Open Repository" }
                <span class="option-hint">{ "GitHub" }</span>
            </a>
        }
    } else {
        html! {}
    };

    let pr_rows = prs
        .iter()
        .map(|pr| {
            let href = pr.url.clone();
            let branch = pr.branch.clone();
            html! {
                <a class="pill-menu-option pr-link" href={href} target="_blank"
                   title={branch.clone()}
                   onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                    { format!("Open PR #{}", pr.number) }
                    <span class="option-hint">{ branch }</span>
                </a>
            }
        })
        .collect::<Html>();

    html! {
        <div class="pill-menu-submenu">
            <button
                type="button"
                class="pill-menu-option repo-submenu-trigger"
                aria-haspopup="true"
                onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
            >
                { "Repo / PRs" }
                <span class="option-hint">
                    { repo_pr_menu_hint(session.repo_url.as_deref(), prs.len()) }
                </span>
            </button>
            <div class="pill-submenu-panel" role="menu">
                { repo_link }
                { pr_rows }
            </div>
        </div>
    }
}

//! SessionRail component - Horizontal carousel of session pills
//!
//! Dropdown pattern matches the send button: always in DOM, toggled by .open class,
//! parent page onclick closes it, toggle button uses stop_propagation.

use crate::components::ShareDialog;
use crate::utils;
use gloo::events::EventListener;
use shared::SessionInfo;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, WheelEvent};
use yew::prelude::*;

/// Props for the SessionRail component
#[derive(Properties, PartialEq)]
pub struct SessionRailProps {
    pub sessions: Vec<SessionInfo>,
    pub focused_index: usize,
    pub awaiting_sessions: HashSet<Uuid>,
    pub paused_sessions: HashSet<Uuid>,
    pub inactive_hidden: bool,
    pub connected_sessions: HashSet<Uuid>,
    pub nav_mode: bool,
    #[prop_or_default]
    pub activity_timestamps: HashMap<Uuid, Vec<(f64, String)>>,
    pub on_select: Callback<usize>,
    pub on_leave: Callback<Uuid>,
    pub on_toggle_pause: Callback<Uuid>,
    pub on_toggle_inactive_hidden: Callback<MouseEvent>,
    pub on_stop: Callback<Uuid>,
}

/// SessionRail - Horizontal carousel of session pills
#[function_component(SessionRail)]
pub fn session_rail(props: &SessionRailProps) -> Html {
    let rail_ref = use_node_ref();
    let menu_session = use_state(|| None::<Uuid>);
    let menu_pos = use_state(|| (0i32, 0i32));
    let stop_confirm = use_state(|| false);
    let copied_id = use_state(|| false);
    let share_session_id = use_state(|| None::<Uuid>);

    // Scroll focused session into view
    {
        let rail_ref = rail_ref.clone();
        let focused_index = props.focused_index;
        use_effect_with(focused_index, move |_| {
            if let Some(rail) = rail_ref.cast::<Element>() {
                if let Some(child) = rail.children().item(focused_index as u32) {
                    child.scroll_into_view();
                }
            }
            || ()
        });
    }

    // Handle wheel event to translate vertical scroll to horizontal
    let on_wheel = {
        let rail_ref = rail_ref.clone();
        Callback::from(move |e: WheelEvent| {
            if let Some(rail) = rail_ref.cast::<HtmlElement>() {
                e.prevent_default();
                let delta = e.delta_y();
                rail.set_scroll_left(rail.scroll_left() + (delta * 3.0) as i32);
            }
        })
    };

    // Close dropdown when clicking anywhere outside the rail container
    {
        let menu_session = menu_session.clone();
        let stop_confirm = stop_confirm.clone();
        let rail_ref = rail_ref.clone();
        let is_open = (*menu_session).is_some();
        use_effect_with(is_open, move |is_open| {
            let listener = if *is_open {
                let document = gloo::utils::document();
                Some(EventListener::new(&document, "click", move |e| {
                    if let Some(rail_el) = rail_ref.cast::<Element>() {
                        if let Some(container) = rail_el.parent_element() {
                            if let Some(target) =
                                e.target().and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                            {
                                if !container.contains(Some(&target)) {
                                    menu_session.set(None);
                                    stop_confirm.set(false);
                                }
                            }
                        }
                    }
                }))
            } else {
                None
            };
            move || drop(listener)
        });
    }

    // Find the session whose menu is open
    let open_session: Option<&SessionInfo> =
        (*menu_session).and_then(|id| props.sessions.iter().find(|s| s.id == id));

    // Build dropdown class + style + content (always rendered, toggled by .open class)
    let is_menu_open = open_session.is_some();
    let dropdown_class = if is_menu_open {
        "pill-dropdown open"
    } else {
        "pill-dropdown"
    };

    let (left, top) = *menu_pos;
    let dropdown_style = if is_menu_open {
        format!("left: {}px; top: {}px;", left, top)
    } else {
        String::new()
    };

    let dropdown_content = if let Some(session) = open_session {
        let is_paused = props.paused_sessions.contains(&session.id);
        let is_connected = props.connected_sessions.contains(&session.id);

        let on_stop = {
            let on_stop = props.on_stop.clone();
            let session_id = session.id;
            let menu_session = menu_session.clone();
            let stop_confirm = stop_confirm.clone();
            Callback::from(move |_: MouseEvent| {
                if *stop_confirm {
                    on_stop.emit(session_id);
                    stop_confirm.set(false);
                    menu_session.set(None);
                } else {
                    stop_confirm.set(true);
                }
            })
        };
        let confirming_stop = *stop_confirm;

        let on_pause = {
            let on_toggle_pause = props.on_toggle_pause.clone();
            let session_id = session.id;
            let menu_session = menu_session.clone();
            Callback::from(move |_: MouseEvent| {
                on_toggle_pause.emit(session_id);
                menu_session.set(None);
            })
        };

        let on_leave = {
            let on_leave = props.on_leave.clone();
            let session_id = session.id;
            let menu_session = menu_session.clone();
            Callback::from(move |_: MouseEvent| {
                on_leave.emit(session_id);
                menu_session.set(None);
            })
        };

        let pause_label = if is_paused {
            "Unpause Session"
        } else {
            "Pause Session"
        };
        let pause_hint = if is_paused {
            "Resume rotation"
        } else {
            "Skip in rotation"
        };

        let stop_option = if is_connected && session.status == shared::SessionStatus::Active {
            let (stop_label, stop_hint) = if confirming_stop {
                ("Click again to confirm", "This will terminate the process")
            } else {
                ("Stop Session", "Terminate process")
            };
            html! {
                <button type="button"
                    class={classes!("pill-menu-option", "stop", confirming_stop.then_some("confirming"))}
                    onclick={on_stop}
                >
                    { stop_label }
                    <span class="option-hint">{ stop_hint }</span>
                </button>
            }
        } else {
            html! {}
        };

        let on_copy_id = {
            let session_id = session.id;
            let copied_id = copied_id.clone();
            Callback::from(move |_: MouseEvent| {
                let window = web_sys::window().expect("no window");
                let clipboard = window.navigator().clipboard();
                let id_str = session_id.to_string();
                let copied_id = copied_id.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ =
                        wasm_bindgen_futures::JsFuture::from(clipboard.write_text(&id_str)).await;
                    copied_id.set(true);
                    let copied_id = copied_id.clone();
                    gloo::timers::callback::Timeout::new(1_500, move || {
                        copied_id.set(false);
                    })
                    .forget();
                });
            })
        };
        let copy_label = if *copied_id { "Copied!" } else { "Session ID" };
        let short_id = &session.id.to_string()[..8];

        let leave_option = if session.my_role != "owner" {
            html! {
                <button type="button" class="pill-menu-option leave" onclick={on_leave}>
                    { "Leave Session" }
                    <span class="option-hint">{ "Remove from your list" }</span>
                </button>
            }
        } else {
            html! {}
        };

        let pr_option = if let Some(ref url) = session.pr_url {
            let pr_number = url.rsplit('/').next().unwrap_or("").to_string();
            let label = if pr_number.is_empty() {
                "Open PR".to_string()
            } else {
                format!("Open PR #{}", pr_number)
            };
            let href = url.clone();
            html! {
                <a class="pill-menu-option pr-link" href={href} target="_blank"
                   onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                    { label }
                    <span class="option-hint">{ "GitHub" }</span>
                </a>
            }
        } else {
            html! {}
        };

        let share_option = if session.my_role == "owner" {
            let share_session_id = share_session_id.clone();
            let session_id = session.id;
            let menu_session = menu_session.clone();
            let on_share = Callback::from(move |_: MouseEvent| {
                share_session_id.set(Some(session_id));
                menu_session.set(None);
            });
            html! {
                <button type="button" class="pill-menu-option share" onclick={on_share}>
                    { "Share Session" }
                    <span class="option-hint">{ "Manage access" }</span>
                </button>
            }
        } else {
            html! {}
        };

        html! {
            <>
                <button
                    type="button"
                    class={classes!("pill-menu-option", "copy-id", (*copied_id).then_some("copied"))}
                    onclick={on_copy_id}
                >
                    { copy_label }
                    <span class="option-hint">{ short_id }</span>
                </button>
                { share_option }
                { pr_option }
                <button
                    type="button"
                    class={classes!("pill-menu-option", "pause", is_paused.then_some("active"))}
                    onclick={on_pause}
                >
                    { pause_label }
                    <span class="option-hint">{ pause_hint }</span>
                </button>
                { leave_option }
                { stop_option }
            </>
        }
    } else {
        html! {}
    };

    // Helper to render a single session pill
    let render_pill = |index: usize,
                       session: &SessionInfo,
                       display_number: Option<usize>|
     -> Html {
        let is_focused = index == props.focused_index;
        let is_awaiting = props.awaiting_sessions.contains(&session.id);
        let is_paused = props.paused_sessions.contains(&session.id);
        let is_connected = props.connected_sessions.contains(&session.id);

        let on_click = {
            let on_select = props.on_select.clone();
            Callback::from(move |_| on_select.emit(index))
        };

        let on_toggle_menu = {
            let menu_session = menu_session.clone();
            let menu_pos = menu_pos.clone();
            let stop_confirm = stop_confirm.clone();
            let copied_id = copied_id.clone();
            let session_id = session.id;
            Callback::from(move |e: MouseEvent| {
                e.stop_propagation();
                stop_confirm.set(false);
                copied_id.set(false);
                if *menu_session == Some(session_id) {
                    menu_session.set(None);
                    return;
                }
                if let Some(el) = e.target_dyn_into::<HtmlElement>() {
                    let rect = el.get_bounding_client_rect();
                    let vw = web_sys::window()
                        .and_then(|w| w.inner_width().ok())
                        .and_then(|v| v.as_f64())
                        .unwrap_or(800.0) as i32;
                    let menu_width = 160; // min-width from CSS
                    let left = (rect.left() as i32).min(vw - menu_width - 8);
                    menu_pos.set((left, rect.bottom() as i32 + 4));
                }
                menu_session.set(Some(session_id));
            })
        };

        let in_nav_mode = props.nav_mode;
        let is_status_disconnected = session.status.as_str() != "active";
        let pill_class = classes!(
            "session-pill",
            if is_focused { Some("focused") } else { None },
            if is_awaiting { Some("awaiting") } else { None },
            if is_paused { Some("paused") } else { None },
            if in_nav_mode { Some("nav-mode") } else { None },
            if is_status_disconnected {
                Some("status-disconnected")
            } else {
                None
            },
        );

        let hostname = &session.hostname;
        let folder = utils::extract_folder(&session.working_directory);

        let connection_class = if is_connected {
            "pill-status connected"
        } else {
            "pill-status disconnected"
        };

        let number_annotation = if in_nav_mode {
            display_number
                .filter(|&n| n < 9)
                .map(|n| format!("{}", n + 1))
        } else {
            None
        };

        // Build sparkline ticks for this session
        let sparkline = {
            let now = js_sys::Date::now();
            let window_ms = 300_000.0; // 5 minutes
            let cutoff = now - window_ms;
            let timestamps = props.activity_timestamps.get(&session.id);

            // Point ticks (non-compaction)
            let ticks: Vec<(f64, &str)> = timestamps
                .map(|ts| {
                    ts.iter()
                        .filter(|(t, msg_type)| {
                            *t > cutoff
                                && !matches!(
                                    msg_type.as_str(),
                                    "compaction_start" | "compaction_end"
                                )
                        })
                        .map(|(t, msg_type)| {
                            let pct = (t - cutoff) / window_ms * 100.0;
                            let css_type = match msg_type.as_str() {
                                "assistant" => "assistant",
                                "user" => "user",
                                "result" => "result",
                                "portal" => "portal",
                                "error" => "error",
                                _ => "other",
                            };
                            (pct, css_type)
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Compaction ranges: pair up start/end events
            let compaction_ranges: Vec<(f64, f64)> = {
                let mut ranges = Vec::new();
                if let Some(ts) = timestamps {
                    let mut pending_start: Option<f64> = None;
                    for (t, msg_type) in ts.iter().filter(|(t, _)| *t > cutoff) {
                        match msg_type.as_str() {
                            "compaction_start" => {
                                pending_start = Some((t - cutoff) / window_ms * 100.0);
                            }
                            "compaction_end" => {
                                let end_pct = (t - cutoff) / window_ms * 100.0;
                                ranges.push((pending_start.take().unwrap_or(0.0), end_pct));
                            }
                            _ => {}
                        }
                    }
                    // In-progress compaction extends to current time
                    if let Some(start_pct) = pending_start {
                        ranges.push((start_pct, 100.0));
                    }
                }
                ranges
            };

            if ticks.is_empty() && compaction_ranges.is_empty() {
                html! {}
            } else {
                html! {
                    <div class="pill-sparkline">
                        { compaction_ranges.iter().map(|(start_pct, end_pct)| {
                            let width = (end_pct - start_pct).max(1.0);
                            let style = format!("left: {:.1}%; width: {:.1}%", start_pct, width);
                            html! { <span class="sparkline-range tick-compaction" {style} /> }
                        }).collect::<Html>() }
                        { ticks.iter().map(|(pct, css_type)| {
                            let style = format!("left: {:.1}%", pct);
                            let class = format!("sparkline-tick tick-{}", css_type);
                            html! { <span {class} {style} /> }
                        }).collect::<Html>() }
                    </div>
                }
            }
        };

        html! {
            <div class={pill_class} onclick={on_click} key={session.id.to_string()}>
                {
                    if let Some(num) = &number_annotation {
                        html! { <span class="pill-number">{ num }</span> }
                    } else {
                        html! {}
                    }
                }
                <span class={connection_class}>
                    { if is_connected { "●" } else { "○" } }
                </span>
                <span class="pill-name" title={session.session_name.clone()}>
                    <span class="pill-folder">{ folder }</span>
                    <span class="pill-hostname">{ hostname }</span>
                    {
                        if let Some(ref branch) = session.git_branch {
                            html! { <span class="pill-branch" title={branch.clone()}>{ branch }</span> }
                        } else {
                            html! {}
                        }
                    }
                </span>
                {
                    if session.agent_type == shared::AgentType::Codex {
                        html! { <span class="pill-agent-badge codex">{ "Codex" }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if is_paused {
                        html! { <span class="pill-paused-badge">{ "ᴾ" }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if session.my_role != "owner" {
                        let role_class = format!("pill-role-badge role-{}", session.my_role);
                        html! { <span class={role_class}>{ &session.my_role }</span> }
                    } else {
                        html! {}
                    }
                }
                <button type="button" class="pill-menu-toggle" onclick={on_toggle_menu}>
                    { "▼" }
                </button>
                { sparkline }
            </div>
        }
    };

    // Split sessions into visible (not paused) vs hidden (paused only)
    let (visible_indices, paused_indices): (Vec<_>, Vec<_>) =
        props.sessions.iter().enumerate().partition(|(_, session)| {
            let is_paused = props.paused_sessions.contains(&session.id);
            !is_paused
        });

    let paused_count = paused_indices.len();
    let visible_count = visible_indices.len();

    // Container with position:relative holds the rail + dropdown.
    // Dropdown uses position:fixed to escape rail overflow clipping.
    // Dropdown uses display:none/.open pattern (same as send button).
    // Clicking anywhere in the container closes the dropdown.
    let on_container_click = {
        let menu_session = menu_session.clone();
        let stop_confirm = stop_confirm.clone();
        Callback::from(move |_: MouseEvent| {
            if (*menu_session).is_some() {
                menu_session.set(None);
                stop_confirm.set(false);
            }
        })
    };

    html! {
        <div class="session-rail-container" onclick={on_container_click}>
            <div class="session-rail" ref={rail_ref} onwheel={on_wheel}>
                { visible_indices.iter().enumerate().map(|(display_idx, (index, session))| {
                    render_pill(*index, session, Some(display_idx))
                }).collect::<Html>() }

                {
                    if paused_count > 0 {
                        let toggle_class = classes!(
                            "session-rail-divider",
                            if props.inactive_hidden { Some("collapsed") } else { None }
                        );
                        html! {
                            <div class={toggle_class} onclick={props.on_toggle_inactive_hidden.clone()}>
                                <span class="divider-line"></span>
                                <button class="divider-toggle" title={if props.inactive_hidden { "Show paused sessions" } else { "Hide paused sessions" }}>
                                    { if props.inactive_hidden {
                                        format!("▶ {}", paused_count)
                                    } else {
                                        "◀".to_string()
                                    }}
                                </button>
                            </div>
                        }
                    } else {
                        html! {}
                    }
                }

                {
                    if !props.inactive_hidden {
                        paused_indices.iter().enumerate().map(|(display_idx, (index, session))| {
                            render_pill(*index, session, Some(visible_count + display_idx))
                        }).collect::<Html>()
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class={dropdown_class} style={dropdown_style}
                onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
            >
                { dropdown_content }
            </div>
            {
                if let Some(session_id) = *share_session_id {
                    let share_session_id = share_session_id.clone();
                    let on_close = Callback::from(move |_| share_session_id.set(None));
                    html! { <ShareDialog {session_id} {on_close} /> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

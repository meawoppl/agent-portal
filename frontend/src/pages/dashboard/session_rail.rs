//! SessionRail component - Horizontal carousel of session pills
//!
//! Dropdown pattern matches the send button: always in DOM, toggled by .open class,
//! parent page onclick closes it, toggle button uses stop_propagation.

use crate::components::{ScheduleDialog, ShareDialog};
use gloo::events::EventListener;
use gloo::timers::callback::Interval;
use shared::{PrRef, SessionInfo};
use std::collections::HashSet;
use uuid::Uuid;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, WheelEvent};
use yew::prelude::*;

mod hooks;
mod menu;
mod pill;
mod sparkline;
use hooks::use_scheduled_task_blocker;
use menu::SessionRailMenu;
use pill::SessionPill;
pub use sparkline::ActivityRef;

const PILL_MENU_MIN_WIDTH_PX: i32 = 160;
const PILL_MENU_ESTIMATED_HEIGHT_PX: i32 = 420;
const PILL_MENU_VIEWPORT_MARGIN_PX: i32 = 8;
const PILL_MENU_TOGGLE_GAP_PX: i32 = 4;

fn clamped_pill_menu_position(
    anchor_left: i32,
    anchor_bottom: i32,
    viewport_width: i32,
    viewport_height: i32,
) -> (i32, i32) {
    let left = anchor_left
        .min(viewport_width - PILL_MENU_MIN_WIDTH_PX - PILL_MENU_VIEWPORT_MARGIN_PX)
        .max(PILL_MENU_VIEWPORT_MARGIN_PX);
    let preferred_top = anchor_bottom + PILL_MENU_TOGGLE_GAP_PX;
    let top = preferred_top
        .min(viewport_height - PILL_MENU_ESTIMATED_HEIGHT_PX - PILL_MENU_VIEWPORT_MARGIN_PX)
        .max(PILL_MENU_VIEWPORT_MARGIN_PX);
    (left, top)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: i64, branch: &str) -> PrRef {
        PrRef {
            number,
            url: format!("https://github.com/example/repo/pull/{number}"),
            branch: branch.to_string(),
        }
    }

    #[test]
    fn sorted_prs_orders_by_number() {
        let prs = vec![pr(42, "z"), pr(7, "a"), pr(13, "m")];

        let sorted = sorted_prs(&prs);

        assert_eq!(
            sorted.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![7, 13, 42]
        );
    }

    #[test]
    fn repo_pr_menu_hint_describes_collapsed_content() {
        assert_eq!(
            repo_pr_menu_hint(Some("https://github.com/r/o"), 0),
            "Repository"
        );
        assert_eq!(
            repo_pr_menu_hint(Some("https://github.com/r/o"), 1),
            "Repository + 1 PR"
        );
        assert_eq!(
            repo_pr_menu_hint(Some("https://github.com/r/o"), 3),
            "Repository + PRs"
        );
        assert_eq!(repo_pr_menu_hint(None, 1), "1 PR");
        assert_eq!(repo_pr_menu_hint(None, 4), "PRs");
    }

    #[test]
    fn pill_menu_position_keeps_menu_inside_right_edge() {
        let (left, top) = clamped_pill_menu_position(760, 100, 800, 900);

        assert_eq!(left, 632);
        assert_eq!(top, 104);
    }

    #[test]
    fn pill_menu_position_opens_upward_near_bottom_edge() {
        let (left, top) = clamped_pill_menu_position(80, 860, 1000, 900);

        assert_eq!(left, 80);
        assert_eq!(top, 472);
    }

    #[test]
    fn pill_menu_position_keeps_minimum_viewport_margin() {
        let (left, top) = clamped_pill_menu_position(-20, -10, 300, 300);

        assert_eq!((left, top), (8, 8));
    }
}

fn sorted_prs(prs: &[PrRef]) -> Vec<PrRef> {
    let mut prs = prs.to_vec();
    prs.sort_by_key(|p| p.number);
    prs
}

fn repo_pr_menu_hint(repo_url: Option<&str>, pr_count: usize) -> &'static str {
    match (repo_url.is_some(), pr_count) {
        (true, 0) => "Repository",
        (true, 1) => "Repository + 1 PR",
        (true, _) => "Repository + PRs",
        (false, 1) => "1 PR",
        (false, _) => "PRs",
    }
}

/// Props for the SessionRail component
#[derive(Properties, PartialEq)]
pub struct SessionRailProps {
    pub sessions: Vec<SessionInfo>,
    pub focused_index: usize,
    pub awaiting_sessions: HashSet<Uuid>,
    pub hidden_sessions: HashSet<Uuid>,
    pub inactive_hidden: bool,
    pub connected_sessions: HashSet<Uuid>,
    pub nav_mode: bool,
    #[prop_or_default]
    pub activity_timestamps: ActivityRef,
    /// Server version string for comparing against client versions
    #[prop_or_default]
    pub server_version: String,
    pub on_select: Callback<usize>,
    pub on_leave: Callback<Uuid>,
    pub on_delete: Callback<Uuid>,
    pub on_toggle_hidden: Callback<Uuid>,
    pub on_toggle_inactive_hidden: Callback<MouseEvent>,
    pub on_stop: Callback<Uuid>,
    pub on_toggle_pause: Callback<(Uuid, bool)>,
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
    let schedule_session = use_state(|| None::<SessionInfo>);
    let stop_has_tasks = use_scheduled_task_blocker(*menu_session, props.sessions.clone());

    // Independent 100 ms tick that drives sparkline redraws.
    // Accumulation happens externally via ActivityRef mutations; this timer
    // is the only thing that causes SessionRail to re-render for sparklines.
    let render_time = use_state(js_sys::Date::now);
    {
        let render_time = render_time.clone();
        use_effect_with((), move |_| {
            let interval = Interval::new(100, move || {
                render_time.set(js_sys::Date::now());
            });
            move || drop(interval)
        });
    }

    // Scroll focused session into view
    {
        let rail_ref = rail_ref.clone();
        let focused_index = props.focused_index;
        use_effect_with(focused_index, move |_| {
            if let Some(rail) = rail_ref.cast::<Element>() {
                let selector = format!("[data-index=\"{}\"]", focused_index);
                if let Ok(Some(child)) = rail.query_selector(&selector) {
                    let opts = web_sys::ScrollIntoViewOptions::new();
                    opts.set_behavior(web_sys::ScrollBehavior::Smooth);
                    opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
                    opts.set_inline(web_sys::ScrollLogicalPosition::Nearest);
                    child.scroll_into_view_with_scroll_into_view_options(&opts);
                }
            }
            || ()
        });
    }

    // Handle wheel event to translate vertical scroll to horizontal.
    // We directly set scrollLeft so that macOS trackpad inertia feels
    // immediate rather than fighting CSS scroll-behavior: smooth.
    let on_wheel = {
        let rail_ref = rail_ref.clone();
        Callback::from(move |e: WheelEvent| {
            if let Some(rail) = rail_ref.cast::<HtmlElement>() {
                // This handler maps the wheel onto the rail's HORIZONTAL scroll,
                // so a plain vertical scroll-wheel drives the top/bottom rail.
                // The left/right rail scrolls *vertically* (`overflow-y: auto`)
                // and has no horizontal overflow — there we must NOT intercept,
                // or `prevent_default` kills the native vertical scroll while we
                // only nudge `scroll_left` (a no-op), leaving the pills unscroll-
                // able. The component doesn't know its orientation, so gate on
                // the axis that actually overflows.
                if rail.scroll_width() <= rail.client_width() {
                    return; // vertical rail (or nothing to scroll) — let native scroll run
                }
                let dx = e.delta_x();
                let dy = e.delta_y();
                // Use whichever axis has the larger delta — this lets both
                // vertical scroll-wheel and horizontal trackpad swipes work
                // naturally. Raw pixel values are used (no multiplier) so the
                // scroll rate matches the rest of macOS.
                let delta = if dx.abs() > dy.abs() { dx } else { dy };
                if delta.abs() < 0.5 {
                    return;
                }
                e.prevent_default();
                let opts = web_sys::ScrollToOptions::new();
                opts.set_left(f64::from(rail.scroll_left()) + delta);
                opts.set_behavior(web_sys::ScrollBehavior::Instant);
                rail.scroll_to_with_scroll_to_options(&opts);
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

    let open_session: Option<SessionInfo> = (*menu_session)
        .and_then(|id| props.sessions.iter().find(|s| s.id == id))
        .cloned();
    let open_session_id = open_session.as_ref().map(|session| session.id);
    let is_menu_session_hidden = open_session_id
        .map(|id| props.hidden_sessions.contains(&id))
        .unwrap_or(false);
    let is_menu_session_connected = open_session_id
        .map(|id| props.connected_sessions.contains(&id))
        .unwrap_or(false);

    let close_menu = {
        let menu_session = menu_session.clone();
        Callback::from(move |()| menu_session.set(None))
    };

    let set_stop_confirm = {
        let stop_confirm = stop_confirm.clone();
        Callback::from(move |value| stop_confirm.set(value))
    };

    let set_copied_id = {
        let copied_id = copied_id.clone();
        Callback::from(move |value| copied_id.set(value))
    };

    let on_share = {
        let share_session_id = share_session_id.clone();
        Callback::from(move |session_id| share_session_id.set(Some(session_id)))
    };

    let on_schedule = {
        let schedule_session = schedule_session.clone();
        Callback::from(move |session| schedule_session.set(Some(session)))
    };

    let on_toggle_pill_menu = {
        let menu_session = menu_session.clone();
        let menu_pos = menu_pos.clone();
        let stop_confirm = stop_confirm.clone();
        let copied_id = copied_id.clone();
        Callback::from(move |(session_id, e): (Uuid, MouseEvent)| {
            e.stop_propagation();
            stop_confirm.set(false);
            copied_id.set(false);
            if *menu_session == Some(session_id) {
                menu_session.set(None);
                return;
            }
            if let Some(el) = e.target_dyn_into::<HtmlElement>() {
                let rect = el.get_bounding_client_rect();
                let (viewport_width, viewport_height) = web_sys::window()
                    .map(|window| {
                        let width = window
                            .inner_width()
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(800.0) as i32;
                        let height = window
                            .inner_height()
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(600.0) as i32;
                        (width, height)
                    })
                    .unwrap_or((800, 600));
                menu_pos.set(clamped_pill_menu_position(
                    rect.left() as i32,
                    rect.bottom() as i32,
                    viewport_width,
                    viewport_height,
                ));
            }
            menu_session.set(Some(session_id));
        })
    };

    // Split sessions into visible vs hidden.
    // Cron sessions default to hidden alongside manually-hidden sessions.
    let (visible_indices, hidden_indices): (Vec<_>, Vec<_>) =
        props.sessions.iter().enumerate().partition(|(_, session)| {
            let is_hidden =
                props.hidden_sessions.contains(&session.id) || session.scheduled_task_id.is_some();
            !is_hidden
        });

    let hidden_count = hidden_indices.len();
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
                    html! {
                        <SessionPill
                            key={session.id.to_string()}
                            index={*index}
                            display_number={Some(display_idx)}
                            session={(*session).clone()}
                            is_focused={*index == props.focused_index}
                            is_awaiting={props.awaiting_sessions.contains(&session.id)}
                            is_hidden={props.hidden_sessions.contains(&session.id)}
                            is_connected={props.connected_sessions.contains(&session.id)}
                            nav_mode={props.nav_mode}
                            server_version={props.server_version.clone()}
                            activity_timestamps={props.activity_timestamps.clone()}
                            render_time={*render_time}
                            on_select={props.on_select.clone()}
                            on_toggle_menu={on_toggle_pill_menu.clone()}
                        />
                    }
                }).collect::<Html>() }

                {
                    if hidden_count > 0 {
                        let toggle_class = classes!(
                            "session-rail-divider",
                            if props.inactive_hidden { Some("collapsed") } else { None }
                        );
                        html! {
                            <div class={toggle_class} onclick={props.on_toggle_inactive_hidden.clone()}>
                                <span class="divider-line"></span>
                                <button class="divider-toggle" title={if props.inactive_hidden { "Show hidden sessions" } else { "Collapse hidden sessions" }}>
                                    { if props.inactive_hidden {
                                        format!("▶ {}", hidden_count)
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
                        hidden_indices.iter().enumerate().map(|(display_idx, (index, session))| {
                            html! {
                                <SessionPill
                                    key={session.id.to_string()}
                                    index={*index}
                                    display_number={Some(visible_count + display_idx)}
                                    session={(*session).clone()}
                                    is_focused={*index == props.focused_index}
                                    is_awaiting={props.awaiting_sessions.contains(&session.id)}
                                    is_hidden={props.hidden_sessions.contains(&session.id)}
                                    is_connected={props.connected_sessions.contains(&session.id)}
                                    nav_mode={props.nav_mode}
                                    server_version={props.server_version.clone()}
                                    activity_timestamps={props.activity_timestamps.clone()}
                                    render_time={*render_time}
                                    on_select={props.on_select.clone()}
                                    on_toggle_menu={on_toggle_pill_menu.clone()}
                                />
                            }
                        }).collect::<Html>()
                    } else {
                        html! {}
                    }
                }
            </div>
            <SessionRailMenu
                session={open_session}
                position={*menu_pos}
                is_hidden={is_menu_session_hidden}
                is_connected={is_menu_session_connected}
                stop_has_tasks={stop_has_tasks}
                confirming_stop={*stop_confirm}
                copied_id={*copied_id}
                on_close={close_menu}
                on_set_stop_confirm={set_stop_confirm}
                on_set_copied_id={set_copied_id}
                on_stop={props.on_stop.clone()}
                on_toggle_hidden={props.on_toggle_hidden.clone()}
                on_toggle_pause={props.on_toggle_pause.clone()}
                on_leave={props.on_leave.clone()}
                on_delete={props.on_delete.clone()}
                on_share={on_share}
                on_schedule={on_schedule}
            />
            {
                if let Some(session_id) = *share_session_id {
                    let share_session_id = share_session_id.clone();
                    let on_close = Callback::from(move |_| share_session_id.set(None));
                    html! { <ShareDialog {session_id} {on_close} /> }
                } else {
                    html! {}
                }
            }
            {
                if let Some(ref session) = *schedule_session {
                    let schedule_session = schedule_session.clone();
                    let on_close = Callback::from(move |_| schedule_session.set(None));
                    html! { <ScheduleDialog session={session.clone()} {on_close} /> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

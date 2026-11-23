//! SessionRail component - Horizontal carousel of session pills
//!
//! Dropdown pattern matches the send button: always in DOM, toggled by .open class,
//! parent page onclick closes it, toggle button uses stop_propagation.

use super::types::PortalSession;
use crate::portal_client::SessionKey;
use gloo::timers::callback::Interval;
use shared::PrRef;
use std::collections::HashSet;
use web_sys::{Element, HtmlElement, WheelEvent};
use yew::prelude::*;

mod broadcast;
mod menu;
mod menu_host;
mod pill;
mod sparkline;
use broadcast::{render_broadcasts, RailAxis};
pub use broadcast::{AgentMessageBroadcast, BroadcastRef};
pub(crate) use menu_host::{use_session_menu, SessionMenuInputs};
use pill::SessionPill;
pub use sparkline::ActivityRef;

/// Poll period of the rail ticker; the frame rate of in-flight orbs.
const RAIL_FRAME_MS: u32 = 100;
/// Minimum spacing of redraws caused by new activity, so a busy turn's
/// stream of events doesn't put the rail back on the frame rate.
const RAIL_ACTIVITY_TICK_MS: f64 = 250.0;
/// Redraw period when nothing is animating and no activity has landed.
const RAIL_IDLE_TICK_MS: f64 = 1_000.0;
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

/// Render one contiguous run of pills (visible or hidden section) into the rail,
/// optionally prefixed with per-host header rows.
///
/// `entries` is the slice of `(index_in_sorted_sessions, session)` pairs to
/// render; `number_offset` is the nav-mode display-number base for the first
/// pill in this run. When `props.group_by_host` is on, a non-focusable header
/// row is emitted before the first pill of each new host group. Headers carry
/// **no** `data-index` or `display_number`, so keyboard numbering / `j`/`k`
/// traversal skip them entirely and stay aligned with the sorted session vec.
///
/// Headers and pills are pushed as *flat siblings* into one keyed list (never
/// nested in fragments) so Yew's keyed reconciliation still preserves each pill
/// instance across reorders — the same guarantee the ungrouped rail relies on.
/// With `group_by_host` off, the emitted list is exactly the keyed pills, i.e.
/// identical to the pre-grouping render path.
fn render_pill_section(
    props: &SessionRailProps,
    entries: &[(usize, &PortalSession)],
    number_offset: usize,
    broadcast_active_ids: (&[SessionKey], &[SessionKey]),
    render_time: f64,
    on_toggle_menu: &Callback<(SessionKey, MouseEvent)>,
    section_key: &str,
) -> Html {
    let (broadcast_senders, broadcast_receivers) = broadcast_active_ids;

    // One keyed pill. `display_idx` is the pill's position within this section
    // (across all host groups), so nav numbering stays sequential.
    let render_pill = |display_idx: usize, index: usize, session: &PortalSession| -> Html {
        let key = session.key;
        html! {
            <SessionPill
                key={key.to_string()}
                index={index}
                display_number={Some(number_offset + display_idx)}
                parent_name={session.info.spawned_by.map(|id| props.sessions.iter()
                    .find(|parent| parent.key.conn == session.key.conn && parent.info.id == id)
                    .map(|parent| parent.info.session_name.clone()).unwrap_or_else(|| id.to_string()))}
                session={session.clone()}
                is_focused={index == props.focused_index}
                is_awaiting={props.awaiting_sessions.contains(&key)}
                is_hidden={props.hidden_sessions.contains(&key)}
                is_connected={props.connected_sessions.contains(&key)}
                nav_mode={props.nav_mode}
                server_version={props.server_version.clone()}
                activity_timestamps={props.activity_timestamps.clone()}
                context_fraction={props.context_fractions.get(&key).copied()}
                progress_fraction={props.progress_fractions.get(&key).copied()}
                is_broadcast_sender={broadcast_senders.contains(&key)}
                is_broadcast_receiver={broadcast_receivers.contains(&key)}
                render_time={render_time}
                on_select={props.on_select.clone()}
                on_toggle_menu={on_toggle_menu.clone()}
            />
        }
    };

    // Ungrouped: a flat keyed list of pills, identical to the pre-grouping path.
    if !props.group_by_host {
        return entries
            .iter()
            .enumerate()
            .map(|(display_idx, (index, session))| render_pill(display_idx, *index, session))
            .collect::<Html>();
    }

    // Grouped: wrap each contiguous same-host run in a tinted section with a
    // single centered/leading header (see `.session-rail-host-group` CSS). The
    // rows are already grouped upstream (`page.rs`), so equal identities are
    // adjacent. One uniform dim tint per group — the header plus that faint
    // background delimit each section on their own; alternating shades just
    // read as noise (#1610).
    let mut groups: Vec<Html> = Vec::new();
    let mut start = 0;
    while start < entries.len() {
        let identity = super::session_order::host_group_key(entries[start].1);
        let mut end = start;
        let mut pills: Vec<Html> = Vec::with_capacity(4);
        while end < entries.len()
            && super::session_order::host_group_key(entries[end].1) == identity
        {
            let (index, session) = entries[end];
            pills.push(render_pill(end, index, session));
            end += 1;
        }
        let label = super::session_order::host_section_label(
            props
                .sessions
                .iter()
                .filter(|session| super::session_order::host_group_key(session) == identity),
        );
        let count = end - start;
        groups.push(html! {
            <div
                key={format!("{section_key}-host-{identity:?}")}
                class="session-rail-host-group"
                style={format!("--session-count: {count}")}
            >
                <div
                    class="session-rail-host-header"
                    title={format!("{label} — {count} session(s)")}
                >
                    <span class="host-header-name">{ label }</span>
                    <span class="host-header-count">{ format!("({count})") }</span>
                </div>
                <div class="session-rail-host-group-pills">
                    { pills.into_iter().collect::<Html>() }
                </div>
            </div>
        });
        start = end;
    }
    groups.into_iter().collect::<Html>()
}

/// Props for the SessionRail component
#[derive(Properties, PartialEq)]
pub struct SessionRailProps {
    pub sessions: Vec<PortalSession>,
    pub focused_index: usize,
    pub awaiting_sessions: HashSet<SessionKey>,
    pub hidden_sessions: HashSet<SessionKey>,
    pub inactive_hidden: bool,
    /// Opt-in: render per-host section headers between pill groups. The order
    /// itself is grouped upstream (see `page.rs`); this flag only controls the
    /// visual header rows so the rail stays in lockstep with nav numbering.
    pub group_by_host: bool,
    pub connected_sessions: HashSet<SessionKey>,
    pub nav_mode: bool,
    #[prop_or_default]
    pub activity_timestamps: ActivityRef,
    /// Per-session context-window fill fraction (session key → 0.0..), derived
    /// from recent turn metrics upstream. Sessions absent from the map render
    /// no context bar.
    #[prop_or_default]
    pub context_fractions: std::collections::HashMap<SessionKey, f64>,
    /// Per-session completion fraction of the agent's progress bars (session
    /// key → 0.0..=1.0). Sessions absent from the map render no pill fill.
    #[prop_or_default]
    pub progress_fractions: std::collections::HashMap<SessionKey, f32>,
    #[prop_or_default]
    pub broadcasts: BroadcastRef,
    pub rail_position: crate::pages::dashboard::RailPosition,
    /// Server version string for comparing against client versions
    #[prop_or_default]
    pub server_version: String,
    /// Threaded through to the close-session confirm hint: with archiving off,
    /// closing a session destroys its transcript rather than filing it.
    #[prop_or_default]
    pub archive_enabled: bool,
    pub on_select: Callback<usize>,
    #[prop_or_default]
    pub on_renamed: Callback<()>,
    pub on_leave: Callback<SessionKey>,
    pub on_delete: Callback<SessionKey>,
    pub on_toggle_hidden: Callback<SessionKey>,
    pub on_toggle_inactive_hidden: Callback<MouseEvent>,
    pub on_stop: Callback<SessionKey>,
    pub on_toggle_pause: Callback<(SessionKey, bool)>,
}

/// SessionRail - Horizontal carousel of session pills
#[function_component(SessionRail)]
pub fn session_rail(props: &SessionRailProps) -> Html {
    let rail_ref = use_node_ref();
    let menu = use_session_menu(SessionMenuInputs {
        sessions: props.sessions.clone(),
        hidden: props.hidden_sessions.clone(),
        connected: props.connected_sessions.clone(),
        archive_enabled: props.archive_enabled,
        on_stop: props.on_stop.clone(),
        on_toggle_hidden: props.on_toggle_hidden.clone(),
        on_toggle_pause: props.on_toggle_pause.clone(),
        on_leave: props.on_leave.clone(),
        on_delete: props.on_delete.clone(),
        on_renamed: props.on_renamed.clone(),
    });

    // Ticker that drives sparkline drift, the compaction flash and the
    // agent-message orbs. Accumulation happens externally via
    // `ActivityRef`/`BroadcastRef` mutations; this timer is the only thing
    // that re-renders `SessionRail` (and so every pill) for them. It polls at
    // the orb frame rate but only commits a new `render_time` while an orb is
    // in flight, shortly after new activity lands, or once per idle period: the
    // sparkline spans five minutes, so a 100 ms step moved it far less than a
    // pixel while re-rendering every pill ten times a second.
    let render_time = use_state(js_sys::Date::now);
    {
        let render_time = render_time.clone();
        let activity = props.activity_timestamps.clone();
        let broadcasts = props.broadcasts.clone();
        use_effect_with((activity.clone(), broadcasts.clone()), move |_| {
            let mut last_render = js_sys::Date::now();
            let mut seen_revision = activity.revision();
            let interval = Interval::new(RAIL_FRAME_MS, move || {
                let now = js_sys::Date::now();
                let revision = activity.revision();
                let elapsed = now - last_render;
                if broadcasts.is_animating(now)
                    || (revision != seen_revision && elapsed >= RAIL_ACTIVITY_TICK_MS)
                    || elapsed >= RAIL_IDLE_TICK_MS
                {
                    last_render = now;
                    seen_revision = revision;
                    render_time.set(now);
                }
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
        let horizontal_rail = matches!(
            props.rail_position,
            crate::pages::dashboard::RailPosition::Top
                | crate::pages::dashboard::RailPosition::Bottom
        );
        Callback::from(move |e: WheelEvent| {
            if !horizontal_rail {
                return;
            }
            if let Some(rail) = rail_ref.cast::<HtmlElement>() {
                // This handler maps the wheel onto the rail's HORIZONTAL scroll,
                // so a plain vertical scroll-wheel drives the top/bottom rail.
                // The left/right rail scrolls vertically (`overflow-y: auto`);
                // never infer orientation from `scroll_width`, because pill
                // padding, host headers, and fixed-position menu geometry can
                // produce a few pixels of horizontal overflow in vertical mode.
                if rail.scroll_width() <= rail.client_width() {
                    return; // nothing to scroll — let native scroll run
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

    // Split sessions into visible vs hidden. Scheduled-task runs get no pill at
    // all; Settings ▸ Schedule lists and links to them.
    let (visible_indices, hidden_indices): (Vec<_>, Vec<_>) = props
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| session.info.scheduled_task_id.is_none())
        .partition(|(_, session)| !props.hidden_sessions.contains(&session.key));

    let hidden_count = hidden_indices.len();
    let visible_count = visible_indices.len();
    let mut rendered_sessions: Vec<(SessionKey, shared::AgentType)> = visible_indices
        .iter()
        .map(|(_, session)| (session.key, session.info.agent_type))
        .collect();
    if !props.inactive_hidden {
        rendered_sessions.extend(
            hidden_indices
                .iter()
                .map(|(_, session)| (session.key, session.info.agent_type)),
        );
    }
    let (broadcast_senders, broadcast_receivers) =
        props.broadcasts.active_session_ids(*render_time);
    let rail_axis = match props.rail_position {
        crate::pages::dashboard::RailPosition::Top
        | crate::pages::dashboard::RailPosition::Bottom => RailAxis::Horizontal,
        crate::pages::dashboard::RailPosition::Left
        | crate::pages::dashboard::RailPosition::Right => RailAxis::Vertical,
    };

    // The container holds the rail and the menu; the dropdown uses
    // position:fixed to escape the rail's overflow clipping.
    html! {
        <div class="session-rail-container">
            <div class="session-rail" ref={rail_ref.clone()} onwheel={on_wheel}>
                {
                    render_pill_section(
                        props,
                        &visible_indices,
                        0,
                        (&broadcast_senders, &broadcast_receivers),
                        *render_time,
                        &menu.toggle,
                        "visible",
                    )
                }

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
                        render_pill_section(
                            props,
                            &hidden_indices,
                            visible_count,
                            (&broadcast_senders, &broadcast_receivers),
                            *render_time,
                            &menu.toggle,
                            "hidden",
                        )
                    } else {
                        html! {}
                    }
                }
            </div>
            { render_broadcasts(&props.broadcasts, &rail_ref, &rendered_sessions, rail_axis, *render_time) }
            { menu.view }
        </div>
    }
}

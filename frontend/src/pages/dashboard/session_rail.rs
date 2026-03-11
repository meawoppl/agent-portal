//! SessionRail component - Horizontal carousel of session pills
//!
//! Dropdown pattern matches the send button: always in DOM, toggled by .open class,
//! parent page onclick closes it, toggle button uses stop_propagation.

use crate::components::ShareDialog;
use crate::utils;
use gloo::events::EventListener;
use gloo::timers::callback::Interval;
use shared::SessionInfo;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use uuid::Uuid;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, WheelEvent};
use yew::prelude::*;

// =============================================================================
// Activity tracking types
// =============================================================================

/// Rolling window for sparkline data (5 minutes).
const SPARKLINE_WINDOW_MS: f64 = 300_000.0;

/// A single point event on the sparkline.
pub struct SparklineTick {
    /// Horizontal position as a percentage of the window width (0–100).
    pub pct: f64,
    /// CSS class suffix (e.g. "assistant", "user", "error").
    pub css_type: &'static str,
}

/// A filled range on the sparkline (compaction or task).
pub struct SparklineRange {
    pub start_pct: f64,
    pub end_pct: f64,
}

/// Everything the sparkline renderer needs for one session.
pub struct SparklineView {
    pub ticks: Vec<SparklineTick>,
    pub compaction_ranges: Vec<SparklineRange>,
    pub task_ranges: Vec<SparklineRange>,
}

impl SparklineView {
    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty() && self.compaction_ranges.is_empty() && self.task_ranges.is_empty()
    }
}

type EventStore = HashMap<Uuid, Vec<(f64, String)>>;

/// Shared activity event buffer.
///
/// Uses pointer-based `PartialEq` so prop changes to the *contents* never
/// cause `SessionRail` to re-render — redraws are driven by its own 100 ms
/// tick timer instead.
#[derive(Clone)]
pub struct ActivityRef(Rc<RefCell<EventStore>>);

impl ActivityRef {
    /// Record a new event, evicting any entries that have fallen outside the
    /// rolling window relative to `timestamp`.
    pub fn push(&self, session_id: Uuid, msg_type: String, timestamp: f64) {
        let cutoff = timestamp - SPARKLINE_WINDOW_MS;
        let mut map = self.0.borrow_mut();
        let events = map.entry(session_id).or_default();
        events.retain(|(t, _)| *t > cutoff);
        events.push((timestamp, msg_type));
    }

    /// Compute the sparkline view for one session at the given wall-clock time.
    pub fn view_for(&self, session_id: Uuid, now: f64) -> SparklineView {
        let cutoff = now - SPARKLINE_WINDOW_MS;
        let map = self.0.borrow();
        let Some(events) = map.get(&session_id) else {
            return SparklineView {
                ticks: vec![],
                compaction_ranges: vec![],
                task_ranges: vec![],
            };
        };

        let ticks = events
            .iter()
            .filter(|(t, kind)| {
                *t > cutoff
                    && !matches!(
                        kind.as_str(),
                        "compaction_start" | "compaction_end" | "task_start" | "task_end"
                    )
            })
            .map(|(t, kind)| SparklineTick {
                pct: (t - cutoff) / SPARKLINE_WINDOW_MS * 100.0,
                css_type: match kind.as_str() {
                    "assistant" => "assistant",
                    "user" => "user",
                    "result" => "result",
                    "portal" => "portal",
                    "error" => "error",
                    _ => "other",
                },
            })
            .collect();

        SparklineView {
            ticks,
            compaction_ranges: extract_ranges(events, cutoff, "compaction_start", "compaction_end"),
            task_ranges: extract_ranges(events, cutoff, "task_start", "task_end"),
        }
    }
}

impl PartialEq for ActivityRef {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Default for ActivityRef {
    fn default() -> Self {
        ActivityRef(Rc::new(RefCell::new(EventStore::new())))
    }
}

/// Pair up `start_tag`/`end_tag` events into percentage ranges.
/// An in-progress range (start with no matching end) extends to 100 %.
fn extract_ranges(
    events: &[(f64, String)],
    cutoff: f64,
    start_tag: &str,
    end_tag: &str,
) -> Vec<SparklineRange> {
    let mut ranges = Vec::new();
    let mut pending_start: Option<f64> = None;
    for (t, kind) in events.iter().filter(|(t, _)| *t > cutoff) {
        let kind = kind.as_str();
        if kind == start_tag {
            pending_start = Some((t - cutoff) / SPARKLINE_WINDOW_MS * 100.0);
        } else if kind == end_tag {
            let end_pct = (t - cutoff) / SPARKLINE_WINDOW_MS * 100.0;
            ranges.push(SparklineRange {
                start_pct: pending_start.take().unwrap_or(0.0),
                end_pct,
            });
        }
    }
    if let Some(start_pct) = pending_start {
        ranges.push(SparklineRange {
            start_pct,
            end_pct: 100.0,
        });
    }
    ranges
}

/// Semver staleness level for a proxy client relative to the server.
enum VersionStaleness {
    /// Same version or no version info available
    Current,
    /// Patch version behind (e.g. 1.3.38 vs 1.3.39)
    PatchBehind,
    /// Minor version behind (e.g. 1.2.0 vs 1.3.0)
    MinorBehind,
    /// Major version behind (e.g. 0.9.0 vs 1.0.0)
    MajorBehind,
}

/// Compare a client version against the server version.
/// Returns the staleness level.
fn version_staleness(client: &str, server: &str) -> VersionStaleness {
    let parse = |s: &str| -> Option<(u64, u64, u64)> {
        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    let Some((cm, cmi, cp)) = parse(client) else {
        return VersionStaleness::Current;
    };
    let Some((sm, smi, sp)) = parse(server) else {
        return VersionStaleness::Current;
    };
    if cm < sm {
        VersionStaleness::MajorBehind
    } else if cmi < smi {
        VersionStaleness::MinorBehind
    } else if cp < sp {
        VersionStaleness::PatchBehind
    } else {
        VersionStaleness::Current
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
    pub on_toggle_hidden: Callback<Uuid>,
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

    // Independent 100 ms tick that drives sparkline redraws.
    // Accumulation happens externally via ActivityRef mutations; this timer
    // is the only thing that causes SessionRail to re-render for sparklines.
    let render_time = use_state(js_sys::Date::now);
    {
        let render_time = render_time.clone();
        use_effect_with((), move |_| {
            let n = Rc::new(std::cell::Cell::new(0u32));
            let interval = Interval::new(100, move || {
                let next = n.get().wrapping_add(1);
                n.set(next);
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
        let is_hidden = props.hidden_sessions.contains(&session.id);
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

        let on_hide = {
            let on_toggle_hidden = props.on_toggle_hidden.clone();
            let session_id = session.id;
            let menu_session = menu_session.clone();
            Callback::from(move |_: MouseEvent| {
                on_toggle_hidden.emit(session_id);
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

        let repo_option = if let Some(ref url) = session.pr_url {
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
        } else if let Some(ref url) = session.repo_url {
            let href = url.clone();
            html! {
                <a class="pill-menu-option pr-link" href={href} target="_blank"
                   onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                    { "Open Repository" }
                    <span class="option-hint">{ "GitHub" }</span>
                </a>
            }
        } else {
            html! {
                <span class="pill-menu-option disabled">
                    { "No Repository Detected" }
                </span>
            }
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
                { repo_option }
                <button
                    type="button"
                    class={classes!("pill-menu-option", "hide", is_hidden.then_some("active"))}
                    onclick={on_hide}
                >
                    { hide_label }
                    <span class="option-hint">{ hide_hint }</span>
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
        let is_hidden = props.hidden_sessions.contains(&session.id);
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
            if is_hidden { Some("hidden") } else { None },
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

        // Build version badge (rendered inline with hostname).
        let version_badge = if let Some(ref cv) = session.client_version {
            if !props.server_version.is_empty() {
                let staleness = version_staleness(cv, &props.server_version);
                let (badge_class, tooltip) = match staleness {
                    VersionStaleness::Current => {
                        ("version-current", format!("v{} — up to date", cv))
                    }
                    VersionStaleness::PatchBehind => (
                        "version-patch",
                        format!(
                            "v{} → v{} (patch update available)",
                            cv, props.server_version
                        ),
                    ),
                    VersionStaleness::MinorBehind => (
                        "version-minor",
                        format!(
                            "v{} → v{} (minor update available)",
                            cv, props.server_version
                        ),
                    ),
                    VersionStaleness::MajorBehind => (
                        "version-major",
                        format!(
                            "v{} → v{} (major update available)",
                            cv, props.server_version
                        ),
                    ),
                };
                html! {
                    <span class={classes!("pill-version-badge", badge_class)}
                        title={tooltip}>
                        { format!("v{}", cv) }
                    </span>
                }
            } else {
                html! {}
            }
        } else {
            html! {}
        };

        // Build sparkline. `render_time` ticks every 100 ms; view_for() does
        // all the windowing and range-pairing at draw time.
        let sparkline = {
            let view = props.activity_timestamps.view_for(session.id, *render_time);
            if view.is_empty() {
                html! {}
            } else {
                html! {
                    <div class="pill-sparkline">
                        { view.compaction_ranges.iter().map(|r| {
                            let width = (r.end_pct - r.start_pct).max(1.0);
                            let style = format!("left: {:.1}%; width: {:.1}%", r.start_pct, width);
                            html! { <span class="sparkline-range tick-compaction" {style} /> }
                        }).collect::<Html>() }
                        { view.task_ranges.iter().map(|r| {
                            let width = (r.end_pct - r.start_pct).max(1.0);
                            let style = format!("left: {:.1}%; width: {:.1}%", r.start_pct, width);
                            html! { <span class="sparkline-range tick-task" {style} /> }
                        }).collect::<Html>() }
                        { view.ticks.iter().map(|t| {
                            let style = format!("left: {:.1}%", t.pct);
                            let class = format!("sparkline-tick tick-{}", t.css_type);
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
                    <span class="pill-hostname-row">
                        <span class="pill-hostname">{ hostname }</span>
                        { version_badge }
                    </span>
                    {
                        if let Some(ref branch) = session.git_branch {
                            html! { <span class="pill-branch" title={branch.clone()}>{ branch }</span> }
                        } else {
                            html! { <span class="pill-branch pill-no-vcs">{ "No VCS" }</span> }
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
                    if session.scheduled_task_id.is_some() {
                        html! { <span class="pill-agent-badge cron">{ "Cron" }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if is_hidden {
                        html! { <span class="pill-hidden-badge">{ "ᴴ" }</span> }
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

    // Split sessions into visible vs hidden
    let (visible_indices, hidden_indices): (Vec<_>, Vec<_>) =
        props.sessions.iter().enumerate().partition(|(_, session)| {
            let is_hidden = props.hidden_sessions.contains(&session.id);
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
                    render_pill(*index, session, Some(display_idx))
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

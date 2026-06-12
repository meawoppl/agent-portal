//! SessionRail component - Horizontal carousel of session pills
//!
//! Dropdown pattern matches the send button: always in DOM, toggled by .open class,
//! parent page onclick closes it, toggle button uses stop_propagation.

use crate::components::{ScheduleDialog, ShareDialog};
use crate::pages::dashboard::session_view::ActivityTag;
use crate::utils::{self, On401};
use gloo::events::EventListener;
use gloo::timers::callback::Interval;
use shared::api::ScheduledTaskListResponse;
use shared::SessionInfo;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
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

type EventStore = HashMap<Uuid, Vec<(f64, ActivityTag)>>;

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
    pub fn push(&self, session_id: Uuid, tag: ActivityTag, timestamp: f64) {
        let cutoff = timestamp - SPARKLINE_WINDOW_MS;
        let mut map = self.0.borrow_mut();
        let events = map.entry(session_id).or_default();
        events.retain(|(t, _)| *t > cutoff);
        events.push((timestamp, tag));
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
            .filter(|(t, tag)| *t > cutoff && !tag.is_range_marker())
            .filter_map(|(t, tag)| {
                tag.tick_css().map(|css_type| SparklineTick {
                    pct: (t - cutoff) / SPARKLINE_WINDOW_MS * 100.0,
                    css_type,
                })
            })
            .collect();

        SparklineView {
            ticks,
            compaction_ranges: extract_ranges(
                events,
                cutoff,
                ActivityTag::is_compaction_start,
                ActivityTag::is_compaction_end,
            ),
            task_ranges: extract_ranges(
                events,
                cutoff,
                ActivityTag::is_task_start,
                ActivityTag::is_task_end,
            ),
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

/// Pair up start/end tag events (selected by the given predicates) into
/// percentage ranges. An in-progress range (start with no matching end)
/// extends to 100 %.
fn extract_ranges(
    events: &[(f64, ActivityTag)],
    cutoff: f64,
    is_start: fn(ActivityTag) -> bool,
    is_end: fn(ActivityTag) -> bool,
) -> Vec<SparklineRange> {
    let mut ranges = Vec::new();
    let mut pending_start: Option<f64> = None;
    for (t, tag) in events.iter().filter(|(t, _)| *t > cutoff) {
        if is_start(*tag) {
            pending_start = Some((t - cutoff) / SPARKLINE_WINDOW_MS * 100.0);
        } else if is_end(*tag) {
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

/// Build a dropdown click handler that runs `action` and then closes the menu.
fn close_then(
    menu_session: UseStateHandle<Option<Uuid>>,
    action: impl Fn() + 'static,
) -> Callback<MouseEvent> {
    Callback::from(move |_: MouseEvent| {
        action();
        menu_session.set(None);
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
    let stop_has_tasks = use_state(|| false);

    // Fetch scheduled task status when dropdown opens for a session
    {
        let stop_has_tasks = stop_has_tasks.clone();
        let menu_id = *menu_session;
        let sessions = props.sessions.clone();
        use_effect_with(menu_id, move |menu_id| {
            if let Some(sid) = menu_id {
                if let Some(session) = sessions.iter().find(|s| s.id == *sid) {
                    let wd = session.working_directory.clone();
                    let stop_has_tasks = stop_has_tasks.clone();
                    spawn_local(async move {
                        if let Ok(data) = utils::fetch_json::<ScheduledTaskListResponse>(
                            "/api/scheduled-tasks",
                            On401::Ignore,
                        )
                        .await
                        {
                            let has = data
                                .tasks
                                .iter()
                                .any(|t| t.working_directory == wd && t.enabled);
                            stop_has_tasks.set(has);
                        }
                    });
                }
            } else {
                stop_has_tasks.set(false);
            }
            || ()
        });
    }

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
        let is_paused = session.paused;
        let session_id = session.id;

        let on_stop = {
            let on_stop = props.on_stop.clone();
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

        // Opens the schedule dialog; used by both the schedule option and the
        // blocked-stop option.
        let open_schedule = close_then(menu_session.clone(), {
            let schedule_session = schedule_session.clone();
            let session = session.clone();
            move || schedule_session.set(Some(session.clone()))
        });

        let on_hide = close_then(menu_session.clone(), {
            let on_toggle_hidden = props.on_toggle_hidden.clone();
            move || on_toggle_hidden.emit(session_id)
        });

        let on_toggle_pause = close_then(menu_session.clone(), {
            let on_toggle_pause = props.on_toggle_pause.clone();
            move || on_toggle_pause.emit((session_id, !is_paused))
        });

        let on_leave = close_then(menu_session.clone(), {
            let on_leave = props.on_leave.clone();
            move || on_leave.emit(session_id)
        });

        let on_delete = close_then(menu_session.clone(), {
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
                if *stop_has_tasks {
                    // Block stop — open schedule dialog so user can delete tasks first
                    menu_option(
                        classes!("stop", "blocked"),
                        "Delete Scheduled Tasks First",
                        "Opens task manager",
                        open_schedule.clone(),
                    )
                } else {
                    let (stop_label, stop_hint) = if confirming_stop {
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
                        classes!("stop", confirming_stop.then_some("confirming")),
                        stop_label,
                        stop_hint,
                        on_stop,
                    )
                }
            } else {
                html! {}
            };

        let on_copy_id = {
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
            menu_option(
                classes!("leave"),
                "Leave Session",
                "Remove from your list",
                on_leave,
            )
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
            let on_share = close_then(menu_session.clone(), {
                let share_session_id = share_session_id.clone();
                move || share_session_id.set(Some(session_id))
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
                    classes!("copy-id", (*copied_id).then_some("copied")),
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
                        { [
                            (&view.compaction_ranges, "sparkline-range tick-compaction"),
                            (&view.task_ranges, "sparkline-range tick-task"),
                        ].into_iter().flat_map(|(ranges, class)| ranges.iter().map(move |r| {
                            let width = (r.end_pct - r.start_pct).max(1.0);
                            let style = format!("left: {:.1}%; width: {:.1}%", r.start_pct, width);
                            html! { <span {class} {style} /> }
                        })).collect::<Html>() }
                        { view.ticks.iter().map(|t| {
                            let style = format!("left: {:.1}%", t.pct);
                            let class = format!("sparkline-tick tick-{}", t.css_type);
                            html! { <span {class} {style} /> }
                        }).collect::<Html>() }
                    </div>
                }
            }
        };

        let watermark_class = match session.agent_type {
            shared::AgentType::Claude => "pill-watermark claude",
            shared::AgentType::Codex => "pill-watermark codex",
        };

        html! {
            <div class={pill_class} onclick={on_click} key={session.id.to_string()} data-index={index.to_string()}>
                <span class={watermark_class} aria-hidden="true" />
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
                // Codex text badge removed — the agent-type watermark behind the
                // pill (anthropic-mark.svg / openai-mark.png) carries this signal.
                {
                    if session.scheduled_task_id.is_some() {
                        html! { <span class="pill-agent-badge cron">{ "Cron" }</span> }
                    } else if session.paused {
                        html! { <span class="pill-agent-badge paused">{ "Paused" }</span> }
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

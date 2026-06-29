use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use uuid::Uuid;
use yew::prelude::*;

use crate::pages::dashboard::session_view::ActivityTag;

/// Rolling window for sparkline data (5 minutes).
const SPARKLINE_WINDOW_MS: f64 = 300_000.0;

/// A single point event on the sparkline.
struct SparklineTick {
    /// Horizontal position as a percentage of the window width (0-100).
    pct: f64,
    /// CSS class suffix (e.g. "assistant", "user", "error").
    css_type: &'static str,
}

/// A filled range on the sparkline (compaction or task).
struct SparklineRange {
    start_pct: f64,
    end_pct: f64,
}

/// Everything the sparkline renderer needs for one session.
struct SparklineView {
    ticks: Vec<SparklineTick>,
    compaction_ranges: Vec<SparklineRange>,
    task_ranges: Vec<SparklineRange>,
}

impl SparklineView {
    fn is_empty(&self) -> bool {
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
    fn view_for(&self, session_id: Uuid, now: f64) -> SparklineView {
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

/// Render the activity sparkline for a session pill.
///
/// `render_time` ticks every 100 ms; `view_for` does all the windowing and
/// range-pairing at draw time.
pub fn render_activity_sparkline(
    activity_timestamps: &ActivityRef,
    session_id: Uuid,
    render_time: f64,
) -> Html {
    let view = activity_timestamps.view_for(session_id, render_time);
    if view.is_empty() {
        return html! {};
    }

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

//! A small "odometer" component that animates an integer climbing toward a
//! target value. Used for the per-turn reasoning-token chip in the turn-metrics
//! footer and for the condensed `thinking` token-count chip.
//!
//! It climbs from its *current* displayed value to the target — not from 0 — so
//! when the target keeps growing (e.g. live `thinking_tokens` estimates arriving
//! one after another), the number keeps ticking upward smoothly instead of
//! snapping back to 0 and re-racing on every update. On mount the current value
//! is `start` (0 by default), so the first reveal rolls start→target. The timer
//! is dropped once the target is reached and on unmount, so historical
//! transcript cards never leave an interval running.

use std::cell::Cell;
use std::rc::Rc;

use gloo::timers::callback::Interval;
use yew::prelude::*;

/// Animation cadence. ~60 frames at 40ms is a ~2.4s climb per leg — a deliberate,
/// slow count-up (keeps the ~25fps smoothness but takes twice as long as the
/// original ~1.2s). Since each new target resumes from the current value, the
/// overall motion stays smooth as estimates stream in.
const FRAMES: i64 = 60;
const FRAME_MS: u32 = 40;

#[derive(Properties, PartialEq)]
pub struct CountUpProps {
    /// Final value to roll up to. Values `<= 0` render a static `0`.
    pub target: i64,
    /// Value the odometer starts from on mount, clamped to `0..=target`.
    /// Lets a chip that logically continues an earlier count — e.g. a
    /// thinking burst whose run was split by a tool call — pick up where its
    /// predecessor left off instead of re-racing from 0. Consulted only on
    /// mount; later target changes resume from the displayed value as before.
    #[prop_or(0)]
    pub start: i64,
    /// Optional label rendered immediately after the number (e.g. `" thinking"`).
    #[prop_or_default]
    pub suffix: AttrValue,
    /// When true, abbreviate large values with `compact_count` (`1234` → `1.2k`).
    /// Leave false for small counts (e.g. pulse counts) that read better in full.
    #[prop_or(false)]
    pub compact: bool,
}

#[function_component(CountUp)]
pub fn count_up(props: &CountUpProps) -> Html {
    let target = props.target.max(0);
    let initial = props.start.clamp(0, target);
    let value = use_state(|| initial);
    // Hold the live interval so we can cancel it both when it reaches the target
    // and on unmount (dropping the `Interval` cancels it).
    let interval = use_mut_ref(|| None::<Interval>);

    {
        let value = value.clone();
        let interval = interval.clone();
        use_effect_with(target, move |&target| {
            // Resume the climb from wherever the display currently sits, not 0,
            // so a growing target keeps ticking up smoothly instead of re-racing.
            let start = *value;
            // Cancel any in-flight leg before starting a new one.
            interval.borrow_mut().take();
            if target > start {
                let step = (((target - start) as f64) / FRAMES as f64).ceil() as i64;
                let cur = Rc::new(Cell::new(start));
                let iv = Interval::new(FRAME_MS, {
                    let value = value.clone();
                    let interval = interval.clone();
                    move || {
                        let next = (cur.get() + step).min(target);
                        cur.set(next);
                        value.set(next);
                        if next >= target {
                            // Reached the target: drop our handle to stop ticking.
                            interval.borrow_mut().take();
                        }
                    }
                });
                *interval.borrow_mut() = Some(iv);
            } else if target < start {
                // Target moved backwards (e.g. a fresh card reusing the slot):
                // snap down rather than animating in reverse.
                value.set(target);
            }
            let interval = interval.clone();
            move || {
                interval.borrow_mut().take();
            }
        });
    }

    let shown = if props.compact {
        crate::components::message_renderer::turn_metrics_footer::compact_count(*value)
    } else {
        value.to_string()
    };

    html! {
        <span class="count-up">{ shown }{ props.suffix.clone() }</span>
    }
}

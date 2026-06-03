//! A small "odometer" component that animates an integer rolling up from 0 to
//! a target value once, on mount. Used for the per-turn reasoning-token chip in
//! the turn-metrics footer and for the condensed `thinking` pulse-count chip.
//!
//! The animation is a one-shot reveal (not a live stream): it ticks from 0 to
//! `target` over a fixed number of frames and then stops, dropping its timer so
//! historical transcript cards don't leave intervals running forever.

use std::cell::Cell;
use std::rc::Rc;

use gloo::timers::callback::Interval;
use yew::prelude::*;

/// Number of animation frames and the per-frame delay. ~20 frames at 28ms is a
/// ~560ms roll — long enough to read as "counting up", short enough not to feel
/// laggy when several land on screen at once.
const FRAMES: i64 = 20;
const FRAME_MS: u32 = 28;

#[derive(Properties, PartialEq)]
pub struct CountUpProps {
    /// Final value to roll up to. Values `<= 0` render a static `0`.
    pub target: i64,
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
    let value = use_state(|| 0i64);
    // Hold the live interval so we can cancel it both when it reaches the target
    // and on unmount (dropping the `Interval` cancels it).
    let interval = use_mut_ref(|| None::<Interval>);

    {
        let value = value.clone();
        let interval = interval.clone();
        use_effect_with(target, move |&target| {
            value.set(0);
            if target > 0 {
                let step = ((target as f64) / FRAMES as f64).ceil() as i64;
                let tick = Rc::new(Cell::new(0i64));
                let iv = Interval::new(FRAME_MS, {
                    let value = value.clone();
                    let interval = interval.clone();
                    move || {
                        let next = (tick.get() + step).min(target);
                        tick.set(next);
                        value.set(next);
                        if next >= target {
                            // Reached the target: drop our handle to stop ticking.
                            interval.borrow_mut().take();
                        }
                    }
                });
                *interval.borrow_mut() = Some(iv);
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

//! Live-updating "time ago" label.
//!
//! Renders a span like "a few seconds ago" / "5 minutes ago" / "2 hours ago"
//! that re-renders every 30 seconds so the label stays accurate without
//! repainting on every parent render. Tooltip shows the exact local time.

use gloo::timers::callback::Interval;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct TimeAgoProps {
    /// ISO 8601 timestamp string (e.g., "2026-05-06T10:32:15Z")
    pub iso: AttrValue,
    #[prop_or_default]
    pub class: Classes,
}

#[function_component(TimeAgo)]
pub fn time_ago(props: &TimeAgoProps) -> Html {
    // tick state forces re-render when the interval fires
    let tick = use_state(|| 0u32);

    {
        let tick = tick.clone();
        use_effect_with((), move |_| {
            let interval = Interval::new(30_000, move || {
                tick.set(*tick + 1);
            });
            // Keep alive for the lifetime of the component
            move || drop(interval)
        });
    }

    let _ = *tick; // ensure dependency

    let iso = &*props.iso;
    let parsed_ms = js_sys::Date::parse(iso);
    if parsed_ms.is_nan() {
        return html! {};
    }

    let now_ms = js_sys::Date::now();
    let diff_secs = ((now_ms - parsed_ms) / 1000.0).max(0.0) as i64;
    let label = format_time_ago(diff_secs);

    // Tooltip: full local time
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(parsed_ms));
    let local_time = date
        .to_locale_string("default", &js_sys::Object::new())
        .as_string()
        .unwrap_or_default();

    html! {
        <span class={classes!("time-ago", props.class.clone())} title={local_time}>
            { label }
        </span>
    }
}

fn format_time_ago(secs: i64) -> String {
    if secs < 10 {
        "just now".to_string()
    } else if secs < 60 {
        "a few seconds ago".to_string()
    } else if secs < 3600 {
        let m = secs / 60;
        if m == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{} minutes ago", m)
        }
    } else if secs < 86_400 {
        let h = secs / 3600;
        if h == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", h)
        }
    } else if secs < 604_800 {
        let d = secs / 86_400;
        if d == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", d)
        }
    } else {
        let w = secs / 604_800;
        if w == 1 {
            "1 week ago".to_string()
        } else {
            format!("{} weeks ago", w)
        }
    }
}

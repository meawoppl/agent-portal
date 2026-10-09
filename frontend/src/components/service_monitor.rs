//! Dashboard-header services monitor: one chip per running portal service
//! (the backend and each of the user's launchers) with host CPU, memory and
//! load, and a click-to-open table with the full readings. Fed by
//! `ServerToClient::ServiceStatsUpdate` through the client WebSocket hook,
//! which also keeps a short CPU history per service for the sparkline.

use std::collections::HashMap;

use shared::{format_bytes_short, ServiceKind, ServiceStats};
use wasm_bindgen::JsCast;
use web_sys::Element;
use yew::prelude::*;

use super::sparkline::Sparkline;

/// A reading older than this is shown dimmed: the launcher's heartbeat is
/// 30 s, so three missed beats means the figures no longer describe now.
pub const STALE_AFTER_MS: u64 = 90_000;

#[derive(Properties, PartialEq)]
pub struct Props {
    /// Current table, backend first.
    pub services: Vec<ServiceStats>,
    /// Host CPU history per service id, oldest → newest.
    pub cpu_history: HashMap<String, Vec<f64>>,
}

/// CSS modifier for a CPU or memory share: calm, busy or hot.
pub fn level_class(percent: f32) -> &'static str {
    if percent >= 85.0 {
        "hot"
    } else if percent >= 60.0 {
        "busy"
    } else {
        "calm"
    }
}

/// Whether a reading is too old to trust, given the current time.
pub fn is_stale(sampled_at_ms: u64, now_ms: u64) -> bool {
    sampled_at_ms == 0 || now_ms.saturating_sub(sampled_at_ms) > STALE_AFTER_MS
}

/// `load 0.8/0.6/0.5 ×8` — the three averages and the core count that
/// gives them scale.
pub fn load_text(load: [f32; 3], cores: u32) -> String {
    format!(
        "{:.2} / {:.2} / {:.2} on {} core{}",
        load[0],
        load[1],
        load[2],
        cores,
        if cores == 1 { "" } else { "s" }
    )
}

/// Short chip name: launchers show their host, the backend its own name.
pub fn chip_name(s: &ServiceStats) -> String {
    match s.kind {
        ServiceKind::Backend => "backend".to_string(),
        ServiceKind::Launcher => short_host(&s.hostname),
    }
}

/// `Matthews-MacBook-Pro-2.local` → `Matthews-MacBook-Pro-2`.
pub fn short_host(hostname: &str) -> String {
    hostname.split('.').next().unwrap_or(hostname).to_string()
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

#[function_component(ServiceMonitor)]
pub fn service_monitor(props: &Props) -> Html {
    let open = use_state(|| false);
    let root = use_node_ref();

    // Close when clicking anywhere outside the monitor.
    {
        let open = open.clone();
        let root = root.clone();
        use_effect_with(*open, move |is_open| {
            let listener = if *is_open {
                let root = root.clone();
                let open = open.clone();
                let closure = wasm_bindgen::closure::Closure::<dyn Fn(web_sys::MouseEvent)>::new(
                    move |e: web_sys::MouseEvent| {
                        let inside = e
                            .target()
                            .and_then(|t| t.dyn_into::<Element>().ok())
                            .zip(root.cast::<Element>())
                            .is_some_and(|(target, root)| root.contains(Some(&target)));
                        if !inside {
                            open.set(false);
                        }
                    },
                );
                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                    let _ = doc.add_event_listener_with_callback(
                        "click",
                        closure.as_ref().unchecked_ref(),
                    );
                }
                Some(closure)
            } else {
                None
            };
            move || {
                if let (Some(closure), Some(doc)) =
                    (listener, web_sys::window().and_then(|w| w.document()))
                {
                    let _ = doc.remove_event_listener_with_callback(
                        "click",
                        closure.as_ref().unchecked_ref(),
                    );
                }
            }
        });
    }

    let on_toggle = {
        let open = open.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            open.set(!*open);
        })
    };

    if props.services.is_empty() {
        return html! {
            <div ref={root} class="service-monitor empty" title="Waiting for the first service readings">
                <span class="service-monitor-label">{ "services" }</span>
                <span class="service-monitor-muted">{ "…" }</span>
            </div>
        };
    }

    let now = now_ms();
    let chips = props.services.iter().map(|s| {
        let cpu = s.sample.host_cpu_percent;
        let mem = s.sample.mem_fraction().map(|f| f * 100.0).unwrap_or(0.0);
        let stale = is_stale(s.sample.sampled_at_ms, now);
        let history = props.cpu_history.get(&s.id).cloned().unwrap_or_default();
        let title = format!(
            "{} on {}: CPU {:.0}%, RAM {} / {} ({:.0}%), load {}{}",
            s.name,
            s.hostname,
            cpu,
            format_bytes_short(s.sample.host_mem_used_bytes),
            format_bytes_short(s.sample.host_mem_total_bytes),
            mem,
            load_text(s.sample.load_avg, s.sample.cores),
            if stale { " — stale" } else { "" }
        );
        html! {
            <span class={classes!("service-chip", stale.then_some("stale"))} title={title}>
                <span class="service-chip-name">{ chip_name(s) }</span>
                <span class={classes!("service-chip-cpu", level_class(cpu))}>{ format!("{cpu:.0}%") }</span>
                <Sparkline values={history} width={44.0} height={14.0} />
                <span class={classes!("service-chip-mem", level_class(mem))}>{ format!("{mem:.0}%") }</span>
                <span class="service-chip-load">{ format!("{:.1}", s.sample.load_avg[0]) }</span>
            </span>
        }
    });

    let rows = props.services.iter().map(|s| {
        let cpu = s.sample.host_cpu_percent;
        let mem = s.sample.mem_fraction().map(|f| f * 100.0).unwrap_or(0.0);
        let stale = is_stale(s.sample.sampled_at_ms, now);
        html! {
            <tr class={classes!(stale.then_some("stale"))}>
                <td class="service-row-name">
                    <span class="service-row-kind">{ match s.kind { ServiceKind::Backend => "backend", ServiceKind::Launcher => "launcher" } }</span>
                    <span>{ &s.name }</span>
                    <span class="service-monitor-muted">{ &s.hostname }</span>
                </td>
                <td class={classes!(level_class(cpu))}>{ format!("{cpu:.0}%") }</td>
                <td>{ format!("{:.0}% of 1 core · {} RSS", s.sample.process_cpu_percent, format_bytes_short(s.sample.process_rss_bytes)) }</td>
                <td class={classes!(level_class(mem))}>{ format!("{} / {}", format_bytes_short(s.sample.host_mem_used_bytes), format_bytes_short(s.sample.host_mem_total_bytes)) }</td>
                <td>{ load_text(s.sample.load_avg, s.sample.cores) }</td>
                <td>{ s.sessions }</td>
                <td class="service-monitor-muted">{ if stale { "stale".to_string() } else { format!("{}s ago", now.saturating_sub(s.sample.sampled_at_ms) / 1000) } }</td>
            </tr>
        }
    });

    html! {
        <div ref={root} class={classes!("service-monitor", open.then_some("open"))} onclick={on_toggle.clone()}>
            <span class="service-monitor-label">{ "services" }</span>
            { for chips }
            <span class={classes!("service-monitor-chevron", open.then_some("open"))} aria-hidden="true">{ "\u{25be}" }</span>
            {
                if *open {
                    html! {
                        <div class="service-monitor-panel" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                            <table>
                                <thead>
                                    <tr>
                                        <th>{ "service" }</th>
                                        <th>{ "host CPU" }</th>
                                        <th>{ "process" }</th>
                                        <th>{ "host RAM" }</th>
                                        <th>{ "load 1 / 5 / 15" }</th>
                                        <th>{ "sessions" }</th>
                                        <th>{ "sampled" }</th>
                                    </tr>
                                </thead>
                                <tbody>{ for rows }</tbody>
                            </table>
                            <div class="service-monitor-muted service-monitor-footnote">
                                { "Host CPU and RAM are the whole machine; process is the service itself. Launchers report every 5 s, or each 30 s heartbeat on older backends." }
                            </div>
                        </div>
                    }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_staleness() {
        assert_eq!(level_class(10.0), "calm");
        assert_eq!(level_class(60.0), "busy");
        assert_eq!(level_class(99.0), "hot");
        assert!(is_stale(0, 1_000));
        assert!(!is_stale(1_000, 50_000));
        assert!(is_stale(1_000, 1_000 + STALE_AFTER_MS + 1));
    }

    #[test]
    fn names_and_load() {
        assert_eq!(
            short_host("Matthews-MacBook-Pro-2.local"),
            "Matthews-MacBook-Pro-2"
        );
        assert_eq!(short_host("bench"), "bench");
        assert_eq!(
            load_text([1.0, 2.0, 3.0], 1),
            "1.00 / 2.00 / 3.00 on 1 core"
        );
        assert_eq!(
            load_text([8.0, 4.0, 2.0], 80),
            "8.00 / 4.00 / 2.00 on 80 cores"
        );
    }
}

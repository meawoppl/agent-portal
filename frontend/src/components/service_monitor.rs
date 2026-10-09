//! Dashboard-header services monitor: one compact chip for the selected
//! portal service (the backend or one of the user's launchers) with host CPU,
//! memory and load, a picker to switch machines, and a click-to-open table
//! with every service's readings. Fed by `ServerToClient::ServiceStatsUpdate`
//! through the client WebSocket hook, which also keeps a short CPU history
//! per service for the sparkline.

use std::collections::HashMap;

use shared::{format_bytes_short, ServiceKind, ServiceStats, SystemSample};
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

/// `0.80 / 0.60 / 0.50` — the three load averages.
pub fn load_text(load: [f32; 3]) -> String {
    format!("{:.2} / {:.2} / {:.2}", load[0], load[1], load[2])
}

/// `×8` — the core count that gives a load average its scale.
pub fn cores_text(cores: u32) -> String {
    format!("\u{d7}{cores}")
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

/// `no readings (v2.14.3)` — what a service that has never reported shows in
/// place of figures. The version matters: a launcher built before system
/// stats existed never reports, and updating it is the fix.
pub fn no_readings_text(version: Option<&str>) -> String {
    match version {
        Some(v) if !v.is_empty() => format!("no readings (v{v})"),
        _ => "no readings".to_string(),
    }
}

/// `3s ago` / `stale` / `no readings` for the sampled column.
pub fn age_text(sample: Option<&SystemSample>, now_ms: u64) -> String {
    match sample {
        None => "no readings".to_string(),
        Some(s) if is_stale(s.sampled_at_ms, now_ms) => "stale".to_string(),
        Some(s) => format!("{}s ago", now_ms.saturating_sub(s.sampled_at_ms) / 1000),
    }
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// localStorage key remembering which service the header shows.
const SELECTION_KEY: &str = "service-monitor.selected";

/// Which service to show: the remembered one if it is still in the table,
/// else the first launcher that has reported (your machines matter more than
/// the server, and a silent one has nothing to show), else any launcher,
/// else the first row.
pub fn pick_selected<'a>(
    services: &'a [ServiceStats],
    remembered: Option<&str>,
) -> Option<&'a ServiceStats> {
    remembered
        .and_then(|id| services.iter().find(|s| s.id == id))
        .or_else(|| {
            services
                .iter()
                .find(|s| s.kind == ServiceKind::Launcher && s.sample.is_some())
        })
        .or_else(|| services.iter().find(|s| s.kind == ServiceKind::Launcher))
        .or_else(|| services.first())
}

fn remembered_selection() -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(SELECTION_KEY)
        .ok()?
}

fn remember_selection(id: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(SELECTION_KEY, id);
    }
}

/// Where the panel's top edge goes: just under the chip, in viewport
/// coordinates. The panel is `position: fixed` so the wide table is centred
/// in the window rather than hung off a chip that may sit near an edge.
fn panel_top_style(root: &NodeRef) -> String {
    let bottom = root
        .cast::<Element>()
        .map(|el| el.get_bounding_client_rect().bottom())
        .unwrap_or(48.0);
    format!("top: {}px", bottom + 6.0)
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

    // Hooks must run unconditionally, so the remembered pick is read before the
    // empty-state early return below.
    let chosen = use_state(remembered_selection);

    if props.services.is_empty() {
        return html! {
            <div ref={root} class="service-monitor empty" title="Waiting for the first service readings">
                <span class="service-monitor-label">{ "services" }</span>
                <span class="service-monitor-muted">{ "\u{2026}" }</span>
            </div>
        };
    }

    // One machine at a time: a picker plus one compact two-line chip.
    let now = now_ms();
    let Some(selected) = pick_selected(&props.services, chosen.as_deref()).cloned() else {
        return html! {};
    };
    let on_pick = {
        let chosen = chosen.clone();
        Callback::from(move |e: Event| {
            let value = e
                .target_unchecked_into::<web_sys::HtmlSelectElement>()
                .value();
            remember_selection(&value);
            chosen.set(Some(value));
        })
    };
    let chip = {
        let s = &selected;
        // A service that has never reported is not "stale" (that dims the
        // chip to near-invisible); its muted "no readings" text says enough.
        let stale = s.sample.is_some_and(|smp| is_stale(smp.sampled_at_ms, now));
        let history = props.cpu_history.get(&s.id).cloned().unwrap_or_default();
        let title = match s.sample {
            Some(smp) => format!(
                "{} on {}: CPU {:.0}%, RAM {} / {} ({:.0}%), load {} {}{}",
                s.name,
                s.hostname,
                smp.host_cpu_percent,
                format_bytes_short(smp.host_mem_used_bytes),
                format_bytes_short(smp.host_mem_total_bytes),
                smp.mem_fraction().map(|f| f * 100.0).unwrap_or(0.0),
                load_text(smp.load_avg),
                cores_text(smp.cores),
                if stale { " \u{2014} stale" } else { "" }
            ),
            None => format!(
                "{} on {}: {}. Update the launcher to see its figures.",
                s.name,
                s.hostname,
                no_readings_text(s.version.as_deref())
            ),
        };
        let stats = match s.sample {
            Some(smp) => {
                let cpu = smp.host_cpu_percent;
                let mem = smp.mem_fraction().map(|f| f * 100.0).unwrap_or(0.0);
                html! {
                    <>
                        <span class={classes!("service-chip-cpu", level_class(cpu))}>{ format!("{cpu:.0}%") }</span>
                        <span class={classes!("service-chip-mem", level_class(mem))}>{ format!("{mem:.0}%") }</span>
                        <span class="service-chip-load">{ format!("{:.1}", smp.load_avg[0]) }</span>
                    </>
                }
            }
            None => html! {
                <span class="service-monitor-muted">{ no_readings_text(s.version.as_deref()) }</span>
            },
        };
        html! {
            <span class={classes!("service-chip", stale.then_some("stale"))} title={title}>
                <span class="service-chip-top">
                    <select class="service-chip-pick" value={s.id.clone()} onchange={on_pick}
                        onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
                        aria-label="Which service to show">
                        { for props.services.iter().map(|o| html! {
                            <option value={o.id.clone()} selected={o.id == s.id}>{ chip_name(o) }</option>
                        }) }
                    </select>
                    <Sparkline values={history} width={44.0} height={14.0} />
                </span>
                <span class="service-chip-stats">{ stats }</span>
            </span>
        }
    };

    let rows = props.services.iter().map(|s| {
        let kind = match s.kind {
            ServiceKind::Backend => "backend",
            ServiceKind::Launcher => "launcher",
        };
        let name = html! {
            <td class="service-row-name">
                <span class="service-row-title">
                    <span class="service-row-kind">{ kind }</span>
                    <span>{ &s.name }</span>
                </span>
                <span class="service-monitor-muted">{ short_host(&s.hostname) }</span>
            </td>
        };
        match s.sample {
            Some(smp) => {
                let cpu = smp.host_cpu_percent;
                let mem = smp.mem_fraction().map(|f| f * 100.0).unwrap_or(0.0);
                let stale = is_stale(smp.sampled_at_ms, now);
                let title = format!(
                    "{} itself: {:.0}% of one core, {} resident{}",
                    s.name,
                    smp.process_cpu_percent,
                    format_bytes_short(smp.process_rss_bytes),
                    s.version
                        .as_deref()
                        .map(|v| format!(" \u{b7} v{v}"))
                        .unwrap_or_default()
                );
                html! {
                    <tr class={classes!(stale.then_some("stale"))} title={title}>
                        { name }
                        <td class={classes!(level_class(cpu))}>{ format!("{cpu:.0}%") }</td>
                        <td class={classes!(level_class(mem))}>{ format!("{} / {}", format_bytes_short(smp.host_mem_used_bytes), format_bytes_short(smp.host_mem_total_bytes)) }</td>
                        <td>{ load_text(smp.load_avg) }<span class="service-monitor-muted">{ format!(" {}", cores_text(smp.cores)) }</span></td>
                        <td>{ s.sessions }</td>
                        <td class="service-monitor-muted">{ age_text(Some(&smp), now) }</td>
                    </tr>
                }
            }
            None => html! {
                <tr class="silent" title="This launcher has never sent a reading; update it to see its figures.">
                    { name }
                    <td colspan="3" class="service-monitor-muted">{ no_readings_text(s.version.as_deref()) }</td>
                    <td>{ s.sessions }</td>
                    <td class="service-monitor-muted">{ "\u{2014}" }</td>
                </tr>
            },
        }
    });

    html! {
        <div ref={root.clone()} class={classes!("service-monitor", open.then_some("open"))} onclick={on_toggle.clone()}
            title="Click for every service's readings">
            { chip }
            <span class={classes!("service-monitor-chevron", open.then_some("open"))} aria-hidden="true">{ "\u{25be}" }</span>
            {
                if *open {
                    html! {
                        <div class="service-monitor-panel" style={panel_top_style(&root)}
                            onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                            <table>
                                <thead>
                                    <tr>
                                        <th>{ "service" }</th>
                                        <th>{ "CPU" }</th>
                                        <th>{ "RAM used / total" }</th>
                                        <th>{ "load 1 / 5 / 15" }</th>
                                        <th>{ "sessions" }</th>
                                        <th>{ "sampled" }</th>
                                    </tr>
                                </thead>
                                <tbody>{ for rows }</tbody>
                            </table>
                            <div class="service-monitor-muted service-monitor-footnote">
                                { "Whole-machine figures; hover a row for the service's own CPU and memory. A launcher with no readings predates system stats: update it." }
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

    fn row(id: &str, kind: ServiceKind, sample: Option<SystemSample>) -> ServiceStats {
        ServiceStats {
            id: id.into(),
            kind,
            name: id.into(),
            hostname: id.into(),
            sessions: 0,
            version: Some("2.15.17".into()),
            sample,
        }
    }

    fn reading(at: u64) -> SystemSample {
        SystemSample {
            sampled_at_ms: at,
            ..SystemSample::default()
        }
    }

    #[test]
    fn selection_prefers_remembered_then_reporting_launcher_then_anything() {
        let table = vec![
            row("backend", ServiceKind::Backend, Some(reading(1))),
            row("silent", ServiceKind::Launcher, None),
            row("l1", ServiceKind::Launcher, Some(reading(1))),
            row("l2", ServiceKind::Launcher, Some(reading(1))),
        ];
        assert_eq!(pick_selected(&table, Some("l2")).unwrap().id, "l2");
        assert_eq!(
            pick_selected(&table, Some("gone")).unwrap().id,
            "l1",
            "stale memory falls back to a launcher that has reported"
        );
        assert_eq!(pick_selected(&table, None).unwrap().id, "l1");
        let all_silent = vec![
            row("backend", ServiceKind::Backend, Some(reading(1))),
            row("silent", ServiceKind::Launcher, None),
        ];
        assert_eq!(pick_selected(&all_silent, None).unwrap().id, "silent");
        let only_backend = vec![row("backend", ServiceKind::Backend, Some(reading(1)))];
        assert_eq!(pick_selected(&only_backend, None).unwrap().id, "backend");
        assert!(pick_selected(&[], Some("l1")).is_none());
    }

    #[test]
    fn names_load_and_ages() {
        assert_eq!(
            short_host("Matthews-MacBook-Pro-2.local"),
            "Matthews-MacBook-Pro-2"
        );
        assert_eq!(short_host("bench"), "bench");
        assert_eq!(load_text([1.0, 2.0, 3.0]), "1.00 / 2.00 / 3.00");
        assert_eq!(cores_text(80), "\u{d7}80");
        assert_eq!(no_readings_text(Some("2.14.3")), "no readings (v2.14.3)");
        assert_eq!(no_readings_text(Some("")), "no readings");
        assert_eq!(no_readings_text(None), "no readings");
        assert_eq!(age_text(None, 10_000), "no readings");
        assert_eq!(age_text(Some(&reading(7_000)), 10_000), "3s ago");
        assert_eq!(
            age_text(Some(&reading(1_000)), 1_000 + STALE_AFTER_MS + 1),
            "stale"
        );
    }
}

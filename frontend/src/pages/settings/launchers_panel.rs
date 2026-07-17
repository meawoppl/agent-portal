use crate::utils::{self, On401};
use gloo_net::http::Request;
use shared::{AppConfig, LauncherInfo};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// How long we wait for a restarting launcher to reconnect before declaring the
/// update failed. Launcher self-update + restart is normally well under a
/// minute; 120s gives generous headroom for a slow release download.
const UPDATE_TIMEOUT_MS: f64 = 120_000.0;
/// How long the green "back online" confirmation lingers before it auto-clears
/// and the row returns to its resting state.
const SUCCESS_AUTOCLEAR_MS: f64 = 6_000.0;

/// Per-launcher update lifecycle, driven entirely by live `LaunchersChanged`
/// pushes (the panel refetches `/api/launchers` on every tick) plus a 1s clock
/// for elapsed-time display and timeouts.
///
/// State machine (keyed by launcher id, one instance per updating launcher):
///
/// ```text
///  click ─▶ Requested ──(drops off list)──▶ Restarting ──(reappears, new ver)──▶ Succeeded ──(6s)──▶ cleared
///              │                                 │        └(reappears, same ver)─▶ SameVersion
///              │(reappears w/ new ver, never     │(120s elapsed)──────────────────▶ TimedOut
///              │ observed the drop)              │
///              └────────────▶ Succeeded          └ TimedOut ──(late reconnect)──▶ Succeeded / SameVersion
/// ```
#[derive(Clone, PartialEq)]
enum UpdatePhase {
    /// POST accepted; the launcher is still in the connected list. Waiting for
    /// it to drop off (or, on a very fast restart, to reappear on a new
    /// version without us ever seeing the gap).
    Requested { since_ms: f64 },
    /// The launcher has dropped off the connected list — it is self-updating
    /// and restarting. Waiting for it to come back.
    Restarting { since_ms: f64 },
    /// Reconnected on a different (or at-/above-target) version. Auto-clears.
    Succeeded { new_version: String, since_ms: f64 },
    /// Reconnected on the same pre-update version — the update likely failed.
    SameVersion { version: String },
    /// Never reconnected within `UPDATE_TIMEOUT_MS`.
    TimedOut,
}

/// A tracked in-flight update. Carries the display metadata captured at request
/// time so we can render a "phantom" row for the launcher while it is absent
/// from the connected list (mid-restart).
#[derive(Clone, PartialEq)]
struct UpdateEntry {
    launcher_name: String,
    hostname: String,
    /// The version the launcher reported *before* the update — the baseline the
    /// reconnect is compared against.
    prev_version: String,
    phase: UpdatePhase,
}

/// Parse a semver-ish "MAJOR.MINOR.PATCH" string into a comparable tuple.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// `current >= target` under semver ordering; `false` if either fails to parse.
fn version_at_least(current: &str, target: &str) -> bool {
    match (parse_version(current), parse_version(target)) {
        (Some(c), Some(t)) => c >= t,
        _ => false,
    }
}

/// Did the launcher come back on a "good" version? Success when the version
/// changed at all from the pre-update baseline, or when it already sits at/above
/// the backend's published target (covers a launcher that was already current).
fn is_successful_return(prev: &str, current: &str, server_version: Option<&str>) -> bool {
    if current != prev {
        return true;
    }
    server_version.is_some_and(|s| version_at_least(current, s))
}

/// Classify a launcher that is present again after (or during) an update.
fn classify_return(
    prev: &str,
    current: &str,
    server_version: Option<&str>,
    now_ms: f64,
) -> UpdatePhase {
    if is_successful_return(prev, current, server_version) {
        UpdatePhase::Succeeded {
            new_version: current.to_string(),
            since_ms: now_ms,
        }
    } else {
        UpdatePhase::SameVersion {
            version: current.to_string(),
        }
    }
}

/// Advance every tracked update by comparing it against the current connected
/// list. Pure so it can be unit-tested; the caller only re-`set`s state when the
/// map actually changed, so this is safe to run on every list tick.
fn reconcile(
    mut states: HashMap<Uuid, UpdateEntry>,
    present: &HashMap<Uuid, String>,
    now_ms: f64,
    server_version: Option<&str>,
) -> HashMap<Uuid, UpdateEntry> {
    let ids: Vec<Uuid> = states.keys().copied().collect();
    for id in ids {
        let entry = match states.get(&id) {
            Some(e) => e.clone(),
            None => continue,
        };
        let present_version = present.get(&id);

        let next: Option<UpdatePhase> = match (&entry.phase, present_version) {
            // Still visible after the request. Catch a restart so fast we never
            // saw the gap; otherwise keep waiting, but time out an update that
            // never even takes the launcher down.
            (UpdatePhase::Requested { since_ms }, Some(v)) => {
                if is_successful_return(&entry.prev_version, v, server_version) {
                    Some(classify_return(
                        &entry.prev_version,
                        v,
                        server_version,
                        now_ms,
                    ))
                } else if now_ms - since_ms > UPDATE_TIMEOUT_MS {
                    Some(UpdatePhase::TimedOut)
                } else {
                    None
                }
            }
            // Dropped off the list → it's restarting.
            (UpdatePhase::Requested { .. }, None) => {
                Some(UpdatePhase::Restarting { since_ms: now_ms })
            }
            // Back after a restart → compare versions.
            (UpdatePhase::Restarting { .. }, Some(v)) => Some(classify_return(
                &entry.prev_version,
                v,
                server_version,
                now_ms,
            )),
            // Still gone → wait until the timeout.
            (UpdatePhase::Restarting { since_ms }, None) => {
                (now_ms - since_ms > UPDATE_TIMEOUT_MS).then_some(UpdatePhase::TimedOut)
            }
            // Success confirmation lingers, then clears.
            (UpdatePhase::Succeeded { since_ms, .. }, _) => {
                if now_ms - since_ms > SUCCESS_AUTOCLEAR_MS {
                    states.remove(&id);
                }
                None
            }
            // Warning persists while present; if it drops again, resume waiting.
            (UpdatePhase::SameVersion { .. }, Some(_)) => None,
            (UpdatePhase::SameVersion { .. }, None) => {
                Some(UpdatePhase::Restarting { since_ms: now_ms })
            }
            // A timed-out launcher that finally reconnects resolves normally —
            // a reappearing launcher must never stay wedged.
            (UpdatePhase::TimedOut, Some(v)) => Some(classify_return(
                &entry.prev_version,
                v,
                server_version,
                now_ms,
            )),
            (UpdatePhase::TimedOut, None) => None,
        };

        if let Some(phase) = next {
            if let Some(e) = states.get_mut(&id) {
                e.phase = phase;
            }
        }
    }
    states
}

/// Whole seconds elapsed since `since_ms`, clamped at 0.
fn elapsed_secs(now_ms: f64, since_ms: f64) -> u64 {
    ((now_ms - since_ms).max(0.0) / 1000.0) as u64
}

#[derive(Properties, PartialEq)]
struct LauncherRowProps {
    launcher: LauncherInfo,
    on_update: Callback<Uuid>,
    /// The launcher's current update lifecycle phase, if any is in flight.
    phase: Option<UpdatePhase>,
}

#[function_component(LauncherRow)]
fn launcher_row(props: &LauncherRowProps) -> Html {
    let l = &props.launcher;
    let on_update = props.on_update.clone();
    let launcher_id = l.launcher_id;

    // A restart in progress (Requested/Restarting) locks the button.
    let in_progress = matches!(
        props.phase,
        Some(UpdatePhase::Requested { .. }) | Some(UpdatePhase::Restarting { .. })
    );

    // Inline two-step confirm instead of a browser `confirm()` popup, which
    // doesn't render well on mobile. Keep the action slots stable so the
    // primary click target doesn't move between the armed/unarmed states.
    let confirming = use_state(|| false);

    let arm = {
        let confirming = confirming.clone();
        Callback::from(move |_| confirming.set(true))
    };
    let cancel = {
        let confirming = confirming.clone();
        Callback::from(move |_| confirming.set(false))
    };
    let confirm = {
        let on_update = on_update.clone();
        let confirming = confirming.clone();
        Callback::from(move |_| {
            confirming.set(false);
            on_update.emit(launcher_id);
        })
    };

    let primary_label = if in_progress {
        "Restarting..."
    } else if *confirming {
        "Confirm Restart"
    } else {
        "Update & Restart"
    };
    let primary_title = if *confirming {
        "Confirm launcher update and restart"
    } else {
        "Pull the latest agent-portal release and restart this launcher"
    };
    let primary_class = classes!(
        "update-button",
        "launcher-update-primary",
        if in_progress {
            "stage-3"
        } else if *confirming {
            "confirming"
        } else {
            "stage-0"
        }
    );
    let primary_onclick = if *confirming { confirm } else { arm };
    let cancel_class = classes!(
        "cancel-button",
        "launcher-update-secondary",
        (!*confirming || in_progress).then_some("is-placeholder")
    );

    // Optional status line beneath the row for terminal/transient phases that
    // apply to a launcher that IS present (came back online, or same version).
    let status = match &props.phase {
        Some(UpdatePhase::Succeeded { new_version, .. }) => Some(html! {
            <tr class="launcher-update-status">
                <td colspan="5" class="launcher-status launcher-status--success">
                    { format!("Back online — v{new_version}") }
                </td>
            </tr>
        }),
        Some(UpdatePhase::SameVersion { version }) => Some(html! {
            <tr class="launcher-update-status">
                <td colspan="5" class="launcher-status launcher-status--warning">
                    { format!(
                        "Came back on the same version (v{version}) — update may have failed."
                    ) }
                </td>
            </tr>
        }),
        Some(UpdatePhase::Requested { .. }) => Some(html! {
            <tr class="launcher-update-status">
                <td colspan="5" class="launcher-status launcher-status--waiting">
                    <span class="spinner-small"></span>
                    { "Update requested — waiting for the launcher to restart…" }
                </td>
            </tr>
        }),
        _ => None,
    };

    html! {
        <>
            <tr class="token-row">
                <td class="token-name">{ &l.launcher_name }</td>
                <td>{ &l.hostname }</td>
                <td>{ format!("v{}", &l.version) }</td>
                <td>{ l.running_sessions }</td>
                <td class="token-actions">
                    <div class="launcher-update-actions">
                        <button
                            class={primary_class}
                            onclick={primary_onclick}
                            title={primary_title}
                            disabled={in_progress}
                        >
                            { primary_label }
                        </button>
                        <button
                            class={cancel_class}
                            onclick={cancel}
                            disabled={!*confirming || in_progress}
                            aria-hidden={(!*confirming || in_progress).to_string()}
                            tabindex={if *confirming && !in_progress { "0" } else { "-1" }}
                        >
                            { "Cancel" }
                        </button>
                    </div>
                </td>
            </tr>
            { status.unwrap_or_default() }
        </>
    }
}

#[derive(Properties, PartialEq)]
struct PhantomLauncherRowProps {
    entry: UpdateEntry,
    /// Current wall clock (ms) for the elapsed-time readout.
    now_ms: f64,
}

/// Renders a launcher that is mid-update and therefore absent from the connected
/// list. Keeps the machine visible (rather than vanishing) while it restarts,
/// and shows either a live "waiting…" indicator or the timeout message.
#[function_component(PhantomLauncherRow)]
fn phantom_launcher_row(props: &PhantomLauncherRowProps) -> Html {
    let e = &props.entry;

    let (badge, status_class, status_body) = match &e.phase {
        UpdatePhase::TimedOut => (
            "offline",
            "launcher-status--failed",
            html! {
                { "Launcher did not reconnect — check the machine \
                   (agent-portal service status / logs)." }
            },
        ),
        // Requested/Restarting/SameVersion while absent are all "waiting to come
        // back" from the user's point of view (the non-Restarting ones are brief
        // transitional states before the next reconcile tick).
        phase => {
            let since = match phase {
                UpdatePhase::Restarting { since_ms }
                | UpdatePhase::Requested { since_ms }
                | UpdatePhase::Succeeded { since_ms, .. } => *since_ms,
                _ => props.now_ms,
            };
            let secs = elapsed_secs(props.now_ms, since);
            (
                "restarting",
                "launcher-status--waiting",
                html! {
                    <>
                        <span class="spinner-small"></span>
                        { format!("Restarting — waiting for launcher to come back… ({secs}s)") }
                    </>
                },
            )
        }
    };

    html! {
        <>
            <tr class="token-row launcher-row--phantom">
                <td class="token-name">{ &e.launcher_name }</td>
                <td>{ &e.hostname }</td>
                <td><span class="launcher-phantom-badge">{ badge }</span></td>
                <td>{ "—" }</td>
                <td class="token-actions">{ "—" }</td>
            </tr>
            <tr class="launcher-update-status">
                <td colspan="5" class={classes!("launcher-status", status_class)}>
                    { status_body }
                </td>
            </tr>
        </>
    }
}

#[function_component(LaunchersPanel)]
pub fn launchers_panel() -> Html {
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let loading = use_state(|| true);
    // Global banner reserved for the POST request itself failing (a live
    // launcher can't be reached). Success/progress is shown per-row.
    let request_error = use_state(|| None::<String>);
    // Per-launcher update lifecycle, keyed by launcher id (#710 follow-up).
    let update_states = use_state(HashMap::<Uuid, UpdateEntry>::new);
    // Backend's published target version, for the same-version detection.
    let server_version = use_state(|| None::<String>);
    // 1s heartbeat driving elapsed-time display and timeout/auto-clear checks.
    let tick = use_state(|| 0u32);

    // Live LaunchersChanged ticks (#710): a launcher restarting while this
    // panel is open shows its reconnect without a page refresh.
    let ws_hook = crate::hooks::use_client_websocket();

    let fetch_launchers = {
        let launchers = launchers.clone();
        let loading = loading.clone();

        Callback::from(move |_| {
            let launchers = launchers.clone();
            let loading = loading.clone();

            spawn_local(async move {
                if let Ok(data) =
                    utils::fetch_json::<Vec<LauncherInfo>>("/api/launchers", On401::Ignore).await
                {
                    launchers.set(data);
                }
                loading.set(false);
            });
        })
    };

    {
        let fetch = fetch_launchers.clone();
        use_effect_with(ws_hook.launcher_event_counter, move |_| {
            fetch.emit(());
            || ()
        });
    }

    // Fetch the backend's published version once so we can tell "came back on a
    // new version" from "came back on the same version".
    {
        let server_version = server_version.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(cfg) = utils::fetch_json::<AppConfig>("/api/config", On401::Ignore).await
                {
                    server_version.set(Some(cfg.server_version));
                }
            });
            || ()
        });
    }

    // 1s heartbeat: only mounted while there is at least one in-flight update
    // would be nicer, but a single always-on 1s tick is cheap and keeps the
    // elapsed readouts and timeout checks simple.
    {
        let tick = tick.clone();
        use_effect_with((), move |_| {
            let interval = gloo::timers::callback::Interval::new(1_000, move || {
                tick.set(tick.wrapping_add(1));
            });
            move || drop(interval)
        });
    }

    // Reconcile the lifecycle map whenever the connected list changes or the
    // clock ticks. Runs pure `reconcile` and only writes back on a real change,
    // so this can't loop (its own writes don't touch the deps).
    {
        let update_states = update_states.clone();
        let list = (*launchers).clone();
        let server = (*server_version).clone();
        use_effect_with((list, *tick, server), move |(list, _tick, server)| {
            let present: HashMap<Uuid, String> = list
                .iter()
                .map(|l| (l.launcher_id, l.version.clone()))
                .collect();
            let now = js_sys::Date::now();
            let current = (*update_states).clone();
            let next = reconcile(current, &present, now, server.as_deref());
            if next != *update_states {
                update_states.set(next);
            }
            || ()
        });
    }

    let on_update = {
        let request_error = request_error.clone();
        let update_states = update_states.clone();
        let launchers = launchers.clone();
        Callback::from(move |launcher_id: Uuid| {
            // Snapshot the pre-update metadata now — we need name/host/version to
            // render the phantom row once the launcher drops off the list.
            let meta = launchers
                .iter()
                .find(|l| l.launcher_id == launcher_id)
                .cloned();
            let Some(l) = meta else {
                return;
            };

            let mut states = (*update_states).clone();
            states.insert(
                launcher_id,
                UpdateEntry {
                    launcher_name: l.launcher_name.clone(),
                    hostname: l.hostname.clone(),
                    prev_version: l.version.clone(),
                    phase: UpdatePhase::Requested {
                        since_ms: js_sys::Date::now(),
                    },
                },
            );
            update_states.set(states);
            request_error.set(None);

            let request_error = request_error.clone();
            let update_states = update_states.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/launchers/{launcher_id}/update"));
                let failure = match Request::post(&url).send().await {
                    Ok(resp) if resp.status() == 200 => None,
                    Ok(resp) => {
                        let text = resp.text().await.unwrap_or_default();
                        Some(format!("Update failed: {} {}", resp.status(), text))
                    }
                    Err(e) => Some(format!("Update request failed: {e:?}")),
                };
                if let Some(msg) = failure {
                    // The update never started — drop the tracked lifecycle so the
                    // row doesn't sit forever in "restarting".
                    let mut states = (*update_states).clone();
                    states.remove(&launcher_id);
                    update_states.set(states);
                    request_error.set(Some(msg));
                }
            });
        })
    };

    // Union of connected launchers and any tracked-but-absent (restarting)
    // launchers, so a machine mid-update stays visible.
    let present_ids: HashSet<Uuid> = launchers.iter().map(|l| l.launcher_id).collect();
    let mut phantom: Vec<(Uuid, UpdateEntry)> = update_states
        .iter()
        .filter(|(id, _)| !present_ids.contains(id))
        .map(|(id, e)| (*id, e.clone()))
        .collect();
    phantom.sort_by(|a, b| a.1.launcher_name.cmp(&b.1.launcher_name));

    let now_ms = js_sys::Date::now();
    let has_rows = !launchers.is_empty() || !phantom.is_empty();

    html! {
        <section class="tokens-section">
            <div class="section-header">
                <h2>{ "Launchers" }</h2>
                <p class="section-description">
                    { "Connected launcher daemons. Authentication tokens do not expire and are \
                       managed automatically." }
                </p>
            </div>

            if let Some(message) = &*request_error {
                <div class="error-message">
                    <p>{ message }</p>
                </div>
            }

            if *loading {
                <div class="loading">
                    <div class="spinner"></div>
                    <p>{ "Loading launchers..." }</p>
                </div>
            } else if !has_rows {
                <div class="empty-state">
                    <p>{ "No launchers connected. Install agent-portal on a machine to get started." }</p>
                </div>
            } else {
                <div class="table-container">
                    <table class="tokens-table">
                        <thead>
                            <tr>
                                <th>{ "Name" }</th>
                                <th>{ "Host" }</th>
                                <th>{ "Version" }</th>
                                <th>{ "Sessions" }</th>
                                <th>{ "Actions" }</th>
                            </tr>
                        </thead>
                        <tbody>
                            { for launchers.iter().map(|l| {
                                let phase = update_states
                                    .get(&l.launcher_id)
                                    .map(|e| e.phase.clone());
                                html! {
                                    <LauncherRow
                                        key={l.launcher_id.to_string()}
                                        launcher={l.clone()}
                                        on_update={on_update.clone()}
                                        phase={phase}
                                    />
                                }
                            }) }
                            { for phantom.iter().map(|(id, e)| {
                                html! {
                                    <PhantomLauncherRow
                                        key={format!("phantom-{id}")}
                                        entry={e.clone()}
                                        now_ms={now_ms}
                                    />
                                }
                            }) }
                        </tbody>
                    </table>
                </div>
            }
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prev: &str, phase: UpdatePhase) -> UpdateEntry {
        UpdateEntry {
            launcher_name: "box".into(),
            hostname: "host".into(),
            prev_version: prev.into(),
            phase,
        }
    }

    fn present(pairs: &[(Uuid, &str)]) -> HashMap<Uuid, String> {
        pairs.iter().map(|(id, v)| (*id, v.to_string())).collect()
    }

    #[test]
    fn requested_to_restarting_when_launcher_drops() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(id, entry("2.5.1", UpdatePhase::Requested { since_ms: 0.0 }));
        let next = reconcile(states, &present(&[]), 1_000.0, Some("2.5.2"));
        assert!(matches!(
            next.get(&id).unwrap().phase,
            UpdatePhase::Restarting { .. }
        ));
    }

    #[test]
    fn restarting_to_succeeded_on_new_version() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(
            id,
            entry("2.5.1", UpdatePhase::Restarting { since_ms: 0.0 }),
        );
        let next = reconcile(states, &present(&[(id, "2.5.2")]), 5_000.0, Some("2.5.2"));
        match &next.get(&id).unwrap().phase {
            UpdatePhase::Succeeded { new_version, .. } => assert_eq!(new_version, "2.5.2"),
            _ => panic!("expected Succeeded phase"),
        }
    }

    #[test]
    fn restarting_to_same_version_warns() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(
            id,
            entry("2.5.1", UpdatePhase::Restarting { since_ms: 0.0 }),
        );
        // Came back on the same, below-target version → failed update.
        let next = reconcile(states, &present(&[(id, "2.5.1")]), 5_000.0, Some("2.5.2"));
        assert!(matches!(
            next.get(&id).unwrap().phase,
            UpdatePhase::SameVersion { .. }
        ));
    }

    #[test]
    fn same_version_but_already_at_target_is_success() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(
            id,
            entry("2.5.2", UpdatePhase::Restarting { since_ms: 0.0 }),
        );
        // Unchanged, but already >= server target → treat as up to date.
        let next = reconcile(states, &present(&[(id, "2.5.2")]), 5_000.0, Some("2.5.2"));
        assert!(matches!(
            next.get(&id).unwrap().phase,
            UpdatePhase::Succeeded { .. }
        ));
    }

    #[test]
    fn restarting_times_out() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(
            id,
            entry("2.5.1", UpdatePhase::Restarting { since_ms: 0.0 }),
        );
        let next = reconcile(
            states,
            &present(&[]),
            UPDATE_TIMEOUT_MS + 1.0,
            Some("2.5.2"),
        );
        assert!(matches!(
            next.get(&id).unwrap().phase,
            UpdatePhase::TimedOut
        ));
    }

    #[test]
    fn timed_out_recovers_on_late_reconnect() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(id, entry("2.5.1", UpdatePhase::TimedOut));
        let next = reconcile(states, &present(&[(id, "2.5.2")]), 999_999.0, Some("2.5.2"));
        assert!(matches!(
            next.get(&id).unwrap().phase,
            UpdatePhase::Succeeded { .. }
        ));
    }

    #[test]
    fn succeeded_auto_clears_after_window() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(
            id,
            entry(
                "2.5.1",
                UpdatePhase::Succeeded {
                    new_version: "2.5.2".into(),
                    since_ms: 0.0,
                },
            ),
        );
        let next = reconcile(
            states,
            &present(&[(id, "2.5.2")]),
            SUCCESS_AUTOCLEAR_MS + 1.0,
            Some("2.5.2"),
        );
        assert!(!next.contains_key(&id), "succeeded entry should auto-clear");
    }

    #[test]
    fn same_version_resumes_waiting_if_it_drops_again() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(
            id,
            entry(
                "2.5.1",
                UpdatePhase::SameVersion {
                    version: "2.5.1".into(),
                },
            ),
        );
        let next = reconcile(states, &present(&[]), 1_000.0, Some("2.5.2"));
        assert!(matches!(
            next.get(&id).unwrap().phase,
            UpdatePhase::Restarting { .. }
        ));
    }

    #[test]
    fn fast_restart_succeeds_without_observed_drop() {
        let id = Uuid::new_v4();
        let mut states = HashMap::new();
        states.insert(id, entry("2.5.1", UpdatePhase::Requested { since_ms: 0.0 }));
        // Never saw it leave; it's already back on a new version.
        let next = reconcile(states, &present(&[(id, "2.5.2")]), 500.0, Some("2.5.2"));
        assert!(matches!(
            next.get(&id).unwrap().phase,
            UpdatePhase::Succeeded { .. }
        ));
    }
}

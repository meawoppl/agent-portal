use crate::portal_client::{ConnId, SessionKey};
use shared::{ServerToClient, ServiceStats, TurnMetrics};
use std::collections::HashMap;
use std::rc::Rc;
use yew::{Reducible, UseReducerHandle, UseStateHandle};

/// Cap for the dashboard's recent-turn ring buffer. Matches the server-side
/// `RECENT_TURN_LIMIT` window: REST hydration returns at most this many, and
/// the live WS path trims the buffer back to this length after every
/// insertion so a long-lived dashboard session can't grow unboundedly.
pub const RECENT_TURN_BUFFER_CAP: usize = 50;
/// Points of host-CPU history kept per service for the header sparkline
/// (readings arrive every 5 s, so about five minutes).
pub const SERVICE_CPU_HISTORY_CAP: usize = 60;

/// The client-WS state that is updated **relative to its previous value**.
///
/// A reducer rather than four `use_state` handles, and that is load-bearing.
/// `UseStateHandle` derefs to an `Rc` snapshot taken in the render that created
/// it, so a handle cloned into the long-lived WS task reads the *mount-time*
/// value forever. Every `read → modify → set` from that task therefore rebuilt
/// from an empty map, collapsing `latest_session_metrics` to a single entry on
/// each frame — the context bar vanishing for every session but the one that
/// just reported. The counters had it worse and quieter: `stale + 1` is the
/// same number every time, so they ticked once and never changed again,
/// silently retiring the refetch-on-launch trigger they exist for.
///
/// `dispatch` applies the action against live state, which is the whole point.
/// It also makes the merge a pure function, so the invariant the old design
/// could only assert in a doc comment is now a test.
///
/// Each user-channel callback supplies its connection identity. Session maps
/// retain it; home-wide service statistics and trends remain home-only.
#[derive(Default, PartialEq)]
pub(crate) struct ClientWsState {
    pub recent_turn_metrics: Vec<TurnMetrics>,
    pub latest_session_metrics: HashMap<SessionKey, TurnMetrics>,
    /// Rail-pill fill per session (`ServerToClient::SessionProgress`): the
    /// completion fraction of the agent's progress bars. Absent = no fill.
    pub session_progress: HashMap<SessionKey, f32>,
    pub launch_event_counter: u32,
    pub launcher_event_counter: u32,
    /// Services monitor table from the newest
    /// `ServerToClient::ServiceStatsUpdate`; backend first.
    pub service_stats: Vec<ServiceStats>,
    /// Host CPU history per service id, oldest → newest, one point per new
    /// reading (a repeated reading is not re-appended).
    pub service_cpu_history: HashMap<String, Vec<(u64, f64)>>,
    /// The last launch or run failure per session, from the owner's user
    /// channel (launch results and agent exits reach only the launcher's
    /// owner, so members' tabs never hold one). Cleared by a successful
    /// launch, a dismissal, or the connection going away.
    pub session_failures: HashMap<SessionKey, SessionFailure>,
}

/// Why a session's agent isn't running, for the banner in its view.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionFailure {
    /// The launcher could not start it; its error, bounded, shown as text.
    LaunchFailed(Option<String>),
    /// It started, then exited almost at once.
    CrashedEarly,
    /// Its conversation to resume was gone on the host.
    ResumeTargetMissing,
    /// It exited with an error.
    Errored,
}

impl SessionFailure {
    pub fn message(&self) -> String {
        match self {
            Self::LaunchFailed(Some(detail)) => format!("The agent failed to start: {detail}"),
            Self::LaunchFailed(None) => "The agent failed to start".into(),
            Self::CrashedEarly => "The agent exited right after starting".into(),
            Self::ResumeTargetMissing => {
                "The agent's previous conversation was not found on its computer".into()
            }
            Self::Errored => "The agent exited with an error".into(),
        }
    }

    /// The failure an exit reports, if any. A completed, stopped or
    /// re-registering exit is not one; none of them clears a recorded
    /// failure either, since only a launch that succeeds proves recovery.
    fn from_exit(reason: shared::SessionExitReason) -> Option<Self> {
        use shared::SessionExitReason as Reason;
        match reason {
            Reason::CrashedEarly => Some(Self::CrashedEarly),
            Reason::ResumeTargetMissing => Some(Self::ResumeTargetMissing),
            Reason::Error => Some(Self::Errored),
            Reason::Completed | Reason::Stopped | Reason::RegistrationRejected => None,
        }
    }
}

/// Longest launcher error shown, in characters.
const MAX_LAUNCH_DETAIL_CHARS: usize = 200;

/// A launcher's error made safe to show as plain text: control characters
/// dropped, bounded without splitting a character, empty meaning none.
fn launch_detail(raw: Option<&str>) -> Option<String> {
    let detail: String = raw?
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LAUNCH_DETAIL_CHARS)
        .collect();
    let detail = detail.trim();
    (!detail.is_empty()).then(|| detail.to_string())
}

pub(crate) enum ClientWsAction {
    /// One `ServerToClient::ServiceStatsUpdate` frame: the full current table.
    ServiceStats(Vec<ServiceStats>),
    /// One live `ServerToClient::TurnMetrics` frame.
    TurnMetrics(Box<TurnMetrics>),
    LinkedMetrics(ConnId, Box<TurnMetrics>),
    LinkedHydrate(ConnId, Vec<TurnMetrics>),
    /// One-shot REST seed: trend rows plus the per-session latest rows.
    Hydrate {
        trend: Vec<TurnMetrics>,
        latest: Vec<TurnMetrics>,
    },
    /// One live `ServerToClient::SessionProgress` frame; `None` clears.
    SessionProgress {
        session_id: SessionKey,
        fraction: Option<f32>,
    },
    /// Drop every pill fill (`ServerToClient::SessionProgressReset`), sent on
    /// each (re)connect ahead of the live values so a fill learned before a
    /// drop can't outlive its bar.
    SessionProgressReset(ConnId),
    LaunchEvent,
    LauncherEvent,
    /// A launch result for a session: success clears its failure.
    LaunchResult {
        session: SessionKey,
        failure: Option<SessionFailure>,
    },
    /// A session's agent exited; records a failure when the reason is one.
    SessionExited {
        session: SessionKey,
        reason: shared::SessionExitReason,
    },
    DismissFailure(SessionKey),
    /// The connection was closed (unlinked, or signed out): nothing learned
    /// on it may outlive it.
    ForgetConn(ConnId),
}

impl Reducible for ClientWsState {
    type Action = ClientWsAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = ClientWsState {
            recent_turn_metrics: self.recent_turn_metrics.clone(),
            latest_session_metrics: self.latest_session_metrics.clone(),
            session_progress: self.session_progress.clone(),
            launch_event_counter: self.launch_event_counter,
            launcher_event_counter: self.launcher_event_counter,
            service_stats: self.service_stats.clone(),
            service_cpu_history: self.service_cpu_history.clone(),
            session_failures: self.session_failures.clone(),
        };
        match action {
            ClientWsAction::ServiceStats(services) => {
                for s in &services {
                    let history = next.service_cpu_history.entry(s.id.clone()).or_default();
                    let Some(sample) = s.sample else {
                        continue;
                    };
                    let newer = history
                        .last()
                        .is_none_or(|(at, _)| sample.sampled_at_ms > *at);
                    if newer && sample.sampled_at_ms > 0 {
                        history.push((sample.sampled_at_ms, f64::from(sample.host_cpu_percent)));
                        if history.len() > SERVICE_CPU_HISTORY_CAP {
                            let excess = history.len() - SERVICE_CPU_HISTORY_CAP;
                            history.drain(..excess);
                        }
                    }
                }
                // Forget services that are gone, so a reconnected launcher
                // with a new id does not drag an old trail along.
                next.service_cpu_history
                    .retain(|id, _| services.iter().any(|s| &s.id == id));
                next.service_stats = services;
            }
            ClientWsAction::LinkedMetrics(conn, metrics) => {
                insert_latest_for(conn, &mut next.latest_session_metrics, *metrics);
            }
            ClientWsAction::LinkedHydrate(conn, metrics) => {
                for metric in metrics {
                    insert_latest_for(conn, &mut next.latest_session_metrics, metric);
                }
            }
            ClientWsAction::TurnMetrics(metrics) => {
                next.recent_turn_metrics = insert_recent_metric(
                    next.recent_turn_metrics,
                    (*metrics).clone(),
                    RECENT_TURN_BUFFER_CAP,
                );
                insert_latest_session_metric(&mut next.latest_session_metrics, *metrics);
            }
            ClientWsAction::Hydrate { trend, latest } => {
                for metric in trend {
                    insert_latest_session_metric(&mut next.latest_session_metrics, metric.clone());
                    next.recent_turn_metrics = insert_recent_metric(
                        next.recent_turn_metrics,
                        metric,
                        RECENT_TURN_BUFFER_CAP,
                    );
                }
                // New backends send each session's newest usable context row
                // explicitly; seeding from the trend rows above keeps the prior
                // best-effort behavior against an old backend.
                for metric in latest {
                    insert_latest_session_metric(&mut next.latest_session_metrics, metric);
                }
            }
            ClientWsAction::SessionProgress {
                session_id,
                fraction,
            } => match fraction {
                Some(fraction) => {
                    next.session_progress.insert(session_id, fraction);
                }
                None => {
                    next.session_progress.remove(&session_id);
                }
            },
            ClientWsAction::SessionProgressReset(conn) => {
                next.session_progress.retain(|key, _| key.conn != conn)
            }
            ClientWsAction::LaunchEvent => {
                next.launch_event_counter = next.launch_event_counter.wrapping_add(1);
            }
            ClientWsAction::LauncherEvent => {
                next.launcher_event_counter = next.launcher_event_counter.wrapping_add(1);
            }
            ClientWsAction::LaunchResult { session, failure } => match failure {
                Some(failure) => {
                    next.session_failures.insert(session, failure);
                }
                None => {
                    next.session_failures.remove(&session);
                }
            },
            ClientWsAction::SessionExited { session, reason } => {
                if let Some(failure) = SessionFailure::from_exit(reason) {
                    next.session_failures.insert(session, failure);
                }
            }
            ClientWsAction::DismissFailure(session) => {
                next.session_failures.remove(&session);
            }
            ClientWsAction::ForgetConn(conn) => {
                next.session_failures.retain(|key, _| key.conn != conn);
            }
        }
        Rc::new(next)
    }
}

pub(crate) fn handle_server_message(
    conn: ConnId,
    msg: ServerToClient,
    shutdown_reason: &UseStateHandle<Option<String>>,
    live: &UseReducerHandle<ClientWsState>,
) {
    match msg {
        // Spend screens read REST totals; the WS push is currently unconsumed.
        ServerToClient::UserSpendUpdate {
            total_spend_usd: _,
            session_costs: _,
        } => {}
        ServerToClient::ServerShutdown {
            reason,
            reconnect_delay_ms,
        } => {
            log::info!(
                "Server shutdown: {} (reconnect in {}ms)",
                reason,
                reconnect_delay_ms
            );
            if conn == ConnId::HOME {
                shutdown_reason.set(Some(reason));
            }
        }
        ServerToClient::TurnMetrics(metrics) => {
            live.dispatch(if conn == ConnId::HOME {
                ClientWsAction::TurnMetrics(metrics)
            } else {
                ClientWsAction::LinkedMetrics(conn, metrics)
            });
        }
        ServerToClient::ServiceStatsUpdate { services } => {
            if conn == ConnId::HOME {
                live.dispatch(ClientWsAction::ServiceStats(services));
            }
        }
        ServerToClient::SessionProgress {
            session_id,
            fraction,
        } => {
            live.dispatch(ClientWsAction::SessionProgress {
                session_id: SessionKey {
                    conn,
                    id: session_id,
                },
                fraction,
            });
        }
        ServerToClient::SessionProgressReset => {
            live.dispatch(ClientWsAction::SessionProgressReset(conn));
        }
        ServerToClient::LaunchSessionResult {
            success,
            error,
            session_id,
            ..
        } => {
            // Push signal from the backend that the launcher finished registering
            // (or failed). Tick the counter so the dashboard refreshes its session
            // list at the exact moment the new row becomes findable.
            if !success {
                log::warn!(
                    "Launch failed: {}",
                    error.as_deref().unwrap_or("(no detail)")
                );
            }
            if let Some(id) = session_id {
                live.dispatch(ClientWsAction::LaunchResult {
                    session: SessionKey { conn, id },
                    failure: (!success)
                        .then(|| SessionFailure::LaunchFailed(launch_detail(error.as_deref()))),
                });
            }
            live.dispatch(ClientWsAction::LaunchEvent);
        }
        ServerToClient::SessionExited {
            session_id, reason, ..
        } => {
            live.dispatch(ClientWsAction::SessionExited {
                session: SessionKey {
                    conn,
                    id: session_id,
                },
                reason,
            });
            // Refetch so the list stops showing the session as running.
            live.dispatch(ClientWsAction::LaunchEvent);
        }
        ServerToClient::LaunchersChanged if conn == ConnId::HOME => {
            // A launcher connected, dropped, or was evicted: tick so open
            // launcher lists refetch at the moment the change is visible.
            live.dispatch(ClientWsAction::LauncherEvent);
        }
        _ => {}
    }
}

pub(crate) fn insert_latest_session_metric(
    latest: &mut HashMap<SessionKey, TurnMetrics>,
    incoming: TurnMetrics,
) {
    insert_latest_for(ConnId::HOME, latest, incoming);
}

fn insert_latest_for(
    conn: ConnId,
    latest: &mut HashMap<SessionKey, TurnMetrics>,
    incoming: TurnMetrics,
) {
    // An agent/version that cannot report a context window must not erase the
    // last usable gauge. The bounded trend ring still retains that raw turn.
    if incoming.context_fraction().is_none() {
        return;
    }
    let key = SessionKey {
        conn,
        id: incoming.session_id,
    };
    let replace = latest
        .get(&key)
        .is_none_or(|existing| incoming.started_at >= existing.started_at);
    if replace {
        latest.insert(key, incoming);
    }
}

/// Insert a new metrics row into a sorted-by-`started_at`-ASC buffer,
/// deduping on `metrics.id` (so a REST-hydrated row plus a live broadcast for
/// the same id collapse into one entry), and trim back to the cap on the
/// oldest side. Pure helper so the recent-buffer logic is unit-testable
/// without spinning up a WebSocket. Returns the new buffer.
pub(crate) fn insert_recent_metric(
    mut buf: Vec<TurnMetrics>,
    incoming: TurnMetrics,
    cap: usize,
) -> Vec<TurnMetrics> {
    // Dedup on id when both sides have one. Rows that come off the WS
    // broadcast always carry the server-assigned id; REST-hydrated rows
    // always carry it too — proxy-emit rows (which have `id == None`) only
    // exist on the proxy → backend side and never reach the frontend.
    if let Some(incoming_id) = incoming.id {
        if let Some(existing) = buf.iter_mut().find(|m| m.id == Some(incoming_id)) {
            *existing = incoming;
            return buf;
        }
    }
    // Insert sorted by `started_at` ASC. binary_search_by keeps the buffer
    // ordered without a full re-sort on every insertion.
    let idx = buf
        .binary_search_by(|m| m.started_at.cmp(&incoming.started_at))
        .unwrap_or_else(|e| e);
    buf.insert(idx, incoming);
    if buf.len() > cap {
        // Drop the oldest entries; the sparkline plots the newest window.
        let excess = buf.len() - cap;
        buf.drain(0..excess);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::TurnMetricsBuilder;
    use uuid::Uuid;

    /// Build a minimal `TurnMetrics` with the fields the recent-buffer
    /// insertion path actually reads (`id`, `started_at`).
    fn sample(id: Option<Uuid>, started_secs: i64) -> TurnMetrics {
        TurnMetricsBuilder::new()
            .id(id)
            .started_secs(started_secs)
            .model(None)
            .service_tier(None)
            .input_tokens(0)
            .output_tokens(0)
            .cache_read(0)
            .cache_creation(0)
            .total_cost_usd(None)
            .stop_reason(None)
            .build()
    }

    /// A session with a usable context window, for the reducer tests.
    fn gauged(session: Uuid, started_secs: i64) -> TurnMetrics {
        let mut m = sample(Some(Uuid::new_v4()), started_secs);
        m.session_id = session;
        m.model_context_window = Some(200_000);
        m.input_tokens = 1_000;
        m
    }

    /// The bug this reducer exists to prevent, and the invariant the field's
    /// own doc comment always claimed: "context status must not disappear when
    /// another session is busy".
    ///
    /// The previous design read → modified → set a `UseStateHandle` captured by
    /// the long-lived WS task. That handle derefs to the snapshot from the
    /// render that made it, so every frame rebuilt the map from the mount-time
    /// value and collapsed it to a single entry — session A's context bar
    /// vanishing the moment session B reported a turn.
    #[test]
    fn session_failures_record_clear_only_on_success_and_stay_per_connection() {
        use shared::SessionExitReason as Reason;
        let id = Uuid::new_v4();
        let home = SessionKey::home(id);
        let linked = SessionKey {
            conn: ConnId::linked(1),
            id,
        };
        let failed = |session| ClientWsAction::LaunchResult {
            session,
            failure: Some(SessionFailure::LaunchFailed(launch_detail(Some(
                "claude: not found\n",
            )))),
        };
        let exited = |session, reason| ClientWsAction::SessionExited { session, reason };

        // An ordinary exit is no failure.
        let state = Rc::new(ClientWsState::default()).reduce(exited(home, Reason::Completed));
        assert!(state.session_failures.is_empty());

        let state = state
            .reduce(failed(home))
            .reduce(exited(linked, Reason::CrashedEarly));
        assert_eq!(
            state.session_failures[&home].message(),
            "The agent failed to start: claude: not found"
        );
        assert_eq!(
            state.session_failures[&linked],
            SessionFailure::CrashedEarly
        );

        // Nothing but a successful launch proves recovery: a stop or a
        // re-registering exit leaves the failure.
        let state = state
            .reduce(exited(home, Reason::Stopped))
            .reduce(exited(home, Reason::RegistrationRejected));
        assert!(state.session_failures.contains_key(&home));
        let state = state.reduce(ClientWsAction::LaunchResult {
            session: home,
            failure: None,
        });
        assert!(!state.session_failures.contains_key(&home));
        assert!(state.session_failures.contains_key(&linked));

        // Dismissal, and a connection going away, clear only their own.
        let state = state
            .reduce(failed(home))
            .reduce(ClientWsAction::DismissFailure(linked));
        assert!(!state.session_failures.contains_key(&linked));
        let state = state
            .reduce(exited(linked, Reason::Error))
            .reduce(ClientWsAction::ForgetConn(ConnId::linked(1)));
        assert_eq!(state.session_failures.keys().collect::<Vec<_>>(), [&home]);
    }

    #[test]
    fn launch_detail_is_bounded_plain_text() {
        assert_eq!(launch_detail(None), None);
        assert_eq!(launch_detail(Some(" \n\u{1b} ")), None);
        let long = "é".repeat(MAX_LAUNCH_DETAIL_CHARS + 50);
        assert_eq!(
            launch_detail(Some(&long)).unwrap().chars().count(),
            MAX_LAUNCH_DETAIL_CHARS
        );
    }

    #[test]
    fn linked_progress_reset_and_metrics_cannot_overwrite_home() {
        let id = Uuid::new_v4();
        let linked = ConnId::linked(1);
        let home_key = SessionKey::home(id);
        let foreign_key = SessionKey { conn: linked, id };
        let state = Rc::new(ClientWsState::default())
            .reduce(ClientWsAction::SessionProgress {
                session_id: home_key,
                fraction: Some(0.2),
            })
            .reduce(ClientWsAction::SessionProgress {
                session_id: foreign_key,
                fraction: Some(0.8),
            })
            .reduce(ClientWsAction::SessionProgressReset(linked));
        assert_eq!(state.session_progress.get(&home_key), Some(&0.2));
        assert!(!state.session_progress.contains_key(&foreign_key));
        let state = state
            .reduce(ClientWsAction::TurnMetrics(Box::new(gauged(id, 100))))
            .reduce(ClientWsAction::LinkedMetrics(
                linked,
                Box::new(gauged(id, 200)),
            ));
        assert_eq!(state.latest_session_metrics.len(), 2);
        assert_eq!(state.recent_turn_metrics.len(), 1);
    }

    #[test]
    fn a_busy_session_does_not_evict_another_sessions_gauge() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let state = Rc::new(ClientWsState::default());

        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(a, 100))));
        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(b, 200))));
        // ...and B stays chatty.
        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(b, 300))));

        assert!(
            state
                .latest_session_metrics
                .contains_key(&SessionKey::home(a)),
            "the quiet session kept its context gauge"
        );
        assert!(state
            .latest_session_metrics
            .contains_key(&SessionKey::home(b)));
        assert_eq!(state.latest_session_metrics.len(), 2);
        assert_eq!(state.recent_turn_metrics.len(), 3, "trend ring accumulates");
    }

    /// Pill fills are per session: one session's progress frame must not
    /// erase another's, and `None` clears only its own.
    #[test]
    fn session_progress_is_kept_per_session_and_none_clears() {
        let (a, b) = (
            SessionKey::home(Uuid::new_v4()),
            SessionKey::home(Uuid::new_v4()),
        );
        let state = Rc::new(ClientWsState::default());
        let state = state.reduce(ClientWsAction::SessionProgress {
            session_id: a,
            fraction: Some(0.25),
        });
        let state = state.reduce(ClientWsAction::SessionProgress {
            session_id: b,
            fraction: Some(0.5),
        });
        let state = state.reduce(ClientWsAction::SessionProgress {
            session_id: b,
            fraction: None,
        });

        assert_eq!(state.session_progress.get(&a), Some(&0.25));
        assert!(!state.session_progress.contains_key(&b));
    }

    /// A reconnect must not keep a fill whose bar expired while the socket was
    /// down: the reset wipes it, and only sessions still running are re-sent.
    #[test]
    fn reset_drops_fills_the_backend_no_longer_reports() {
        let (a, b) = (
            SessionKey::home(Uuid::new_v4()),
            SessionKey::home(Uuid::new_v4()),
        );
        let state = Rc::new(ClientWsState::default());
        let state = state.reduce(ClientWsAction::SessionProgress {
            session_id: a,
            fraction: Some(0.25),
        });
        let state = state.reduce(ClientWsAction::SessionProgress {
            session_id: b,
            fraction: Some(0.5),
        });

        let state = state.reduce(ClientWsAction::SessionProgressReset(ConnId::HOME));
        let state = state.reduce(ClientWsAction::SessionProgress {
            session_id: b,
            fraction: Some(0.5),
        });

        assert!(!state.session_progress.contains_key(&a));
        assert_eq!(state.session_progress.get(&b), Some(&0.5));
    }

    /// The same stale-snapshot bug made the launch/launcher counters tick
    /// exactly once — `stale + 1` is the same number every frame, and these
    /// counters are documented as meaningful only when they *change*, so the
    /// refetch they trigger silently stopped firing after the first event.
    #[test]
    fn event_counters_keep_advancing() {
        let state = Rc::new(ClientWsState::default());
        let state = state.reduce(ClientWsAction::LaunchEvent);
        let state = state.reduce(ClientWsAction::LaunchEvent);
        let state = state.reduce(ClientWsAction::LauncherEvent);

        assert_eq!(state.launch_event_counter, 2, "two launches, two ticks");
        assert_eq!(state.launcher_event_counter, 1);
    }

    /// REST hydration and the live stream must compose: a session seeded at
    /// mount keeps its gauge once live frames start arriving for others.
    #[test]
    fn hydration_then_live_frames_compose() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let state = Rc::new(ClientWsState::default()).reduce(ClientWsAction::Hydrate {
            trend: vec![gauged(a, 100)],
            latest: vec![gauged(a, 100)],
        });
        assert!(state
            .latest_session_metrics
            .contains_key(&SessionKey::home(a)));

        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(b, 200))));
        assert!(
            state
                .latest_session_metrics
                .contains_key(&SessionKey::home(a)),
            "hydrated gauge survives the first live frame from another session"
        );
    }

    #[test]
    fn insert_into_empty_buffer() {
        let id = Uuid::new_v4();
        let buf = insert_recent_metric(Vec::new(), sample(Some(id), 100), 50);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].id, Some(id));
    }

    #[test]
    fn insert_keeps_ascending_order() {
        let mut buf = Vec::new();
        buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), 200), 50);
        buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), 100), 50);
        buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), 150), 50);
        let secs: Vec<i64> = buf.iter().map(|m| m.started_at.timestamp()).collect();
        assert_eq!(secs, vec![100, 150, 200]);
    }

    #[test]
    fn dedup_on_id_collapses_repeat_broadcast() {
        // A REST-hydrated row followed by a live WS broadcast for the same
        // id should produce a single buffer entry — the live row replaces
        // the REST one in place. This guards against the row count drifting
        // upward on every reconnect.
        let id = Uuid::new_v4();
        let buf = Vec::new();
        let buf = insert_recent_metric(buf, sample(Some(id), 100), 50);
        let buf = insert_recent_metric(buf, sample(Some(id), 100), 50);
        assert_eq!(buf.len(), 1);
    }

    #[test]
    fn cap_drops_oldest_when_exceeded() {
        // Insert 5 entries with a cap of 3 — only the newest 3 survive.
        let mut buf = Vec::new();
        for t in [10, 20, 30, 40, 50] {
            buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), t), 3);
        }
        assert_eq!(buf.len(), 3);
        let secs: Vec<i64> = buf.iter().map(|m| m.started_at.timestamp()).collect();
        assert_eq!(secs, vec![30, 40, 50]);
    }

    #[test]
    fn latest_per_session_survives_unrelated_ring_eviction() {
        let quiet = Uuid::new_v4();
        let busy = Uuid::new_v4();
        let mut latest = HashMap::new();
        let mut quiet_metric = sample(Some(Uuid::new_v4()), 10);
        quiet_metric.session_id = quiet;
        quiet_metric.model_context_window = Some(200_000);
        quiet_metric.context_snapshot_tokens = Some(50_000);
        insert_latest_session_metric(&mut latest, quiet_metric);

        let mut ring = Vec::new();
        for t in 20..=80 {
            let mut metric = sample(Some(Uuid::new_v4()), t);
            metric.session_id = busy;
            metric.model_context_window = Some(200_000);
            metric.context_snapshot_tokens = Some(t * 100);
            insert_latest_session_metric(&mut latest, metric.clone());
            ring = insert_recent_metric(ring, metric, 50);
        }

        assert!(ring.iter().all(|metric| metric.session_id != quiet));
        assert_eq!(
            latest
                .get(&SessionKey::home(quiet))
                .unwrap()
                .started_at
                .timestamp(),
            10
        );
    }

    #[test]
    fn unknown_context_does_not_erase_last_usable_status() {
        let session_id = Uuid::new_v4();
        let mut latest = HashMap::new();
        let mut usable = sample(Some(Uuid::new_v4()), 10);
        usable.session_id = session_id;
        usable.model_context_window = Some(200_000);
        usable.context_snapshot_tokens = Some(50_000);
        insert_latest_session_metric(&mut latest, usable);

        let mut unknown = sample(Some(Uuid::new_v4()), 20);
        unknown.session_id = session_id;
        insert_latest_session_metric(&mut latest, unknown);

        assert_eq!(
            latest
                .get(&SessionKey::home(session_id))
                .unwrap()
                .started_at
                .timestamp(),
            10
        );
    }
}

#[cfg(test)]
mod service_stats_tests {
    use super::*;
    use shared::{ServiceKind, SystemSample};

    fn row(id: &str, at: u64, cpu: f32) -> ServiceStats {
        ServiceStats {
            id: id.into(),
            kind: ServiceKind::Launcher,
            name: id.into(),
            hostname: "h".into(),
            sessions: 0,
            version: None,
            sample: (at > 0).then_some(SystemSample {
                sampled_at_ms: at,
                host_cpu_percent: cpu,
                ..SystemSample::default()
            }),
        }
    }

    #[test]
    fn history_appends_only_new_readings_and_forgets_gone_services() {
        let mut state = Rc::new(ClientWsState::default());
        state = state.reduce(ClientWsAction::ServiceStats(vec![
            row("a", 1, 10.0),
            row("b", 1, 5.0),
        ]));
        // Same reading again (periodic re-broadcast): no new point.
        state = state.reduce(ClientWsAction::ServiceStats(vec![row("a", 1, 10.0)]));
        state = state.reduce(ClientWsAction::ServiceStats(vec![row("a", 2, 20.0)]));
        assert_eq!(state.service_stats.len(), 1);
        assert_eq!(state.service_cpu_history["a"], vec![(1, 10.0), (2, 20.0)]);
        assert!(
            !state.service_cpu_history.contains_key("b"),
            "b left the table"
        );
        // A launcher that has not reported (no sample) adds nothing.
        state = state.reduce(ClientWsAction::ServiceStats(vec![row("a", 0, 99.0)]));
        assert_eq!(state.service_cpu_history["a"].len(), 2);
    }

    #[test]
    fn history_is_capped() {
        let mut state = Rc::new(ClientWsState::default());
        for i in 1..=(SERVICE_CPU_HISTORY_CAP as u64 + 10) {
            state = state.reduce(ClientWsAction::ServiceStats(vec![row("a", i, 1.0)]));
        }
        let h = &state.service_cpu_history["a"];
        assert_eq!(h.len(), SERVICE_CPU_HISTORY_CAP);
        assert_eq!(h.first().unwrap().0, 11);
    }
}

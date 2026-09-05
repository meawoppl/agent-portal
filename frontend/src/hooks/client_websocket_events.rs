use shared::{ServerToClient, TurnMetrics};
use std::collections::HashMap;
use std::rc::Rc;
use uuid::Uuid;
use yew::{Reducible, UseReducerHandle, UseStateHandle};

/// Cap for the dashboard's recent-turn ring buffer. Matches the server-side
/// `RECENT_TURN_LIMIT` window: REST hydration returns at most this many, and
/// the live WS path trims the buffer back to this length after every
/// insertion so a long-lived dashboard session can't grow unboundedly.
pub const RECENT_TURN_BUFFER_CAP: usize = 50;

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
#[derive(Default, PartialEq)]
pub(crate) struct ClientWsState {
    pub recent_turn_metrics: Vec<TurnMetrics>,
    pub latest_session_metrics: HashMap<Uuid, TurnMetrics>,
    pub launch_event_counter: u32,
    pub launcher_event_counter: u32,
}

pub(crate) enum ClientWsAction {
    /// One live `ServerToClient::TurnMetrics` frame.
    TurnMetrics(Box<TurnMetrics>),
    /// One-shot REST seed: trend rows plus the per-session latest rows.
    Hydrate {
        trend: Vec<TurnMetrics>,
        latest: Vec<TurnMetrics>,
    },
    LaunchEvent,
    LauncherEvent,
}

impl Reducible for ClientWsState {
    type Action = ClientWsAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        let mut next = ClientWsState {
            recent_turn_metrics: self.recent_turn_metrics.clone(),
            latest_session_metrics: self.latest_session_metrics.clone(),
            launch_event_counter: self.launch_event_counter,
            launcher_event_counter: self.launcher_event_counter,
        };
        match action {
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
            ClientWsAction::LaunchEvent => {
                next.launch_event_counter = next.launch_event_counter.wrapping_add(1);
            }
            ClientWsAction::LauncherEvent => {
                next.launcher_event_counter = next.launcher_event_counter.wrapping_add(1);
            }
        }
        Rc::new(next)
    }
}

pub(crate) fn handle_server_message(
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
            shutdown_reason.set(Some(reason));
        }
        ServerToClient::TurnMetrics(metrics) => {
            live.dispatch(ClientWsAction::TurnMetrics(metrics));
        }
        ServerToClient::LaunchSessionResult { success, error, .. } => {
            // Push signal from the backend that the launcher finished registering
            // (or failed). Tick the counter so the dashboard refreshes its session
            // list at the exact moment the new row becomes findable.
            if !success {
                log::warn!(
                    "Launch failed: {}",
                    error.as_deref().unwrap_or("(no detail)")
                );
            }
            live.dispatch(ClientWsAction::LaunchEvent);
        }
        ServerToClient::LaunchersChanged => {
            // A launcher connected, dropped, or was evicted: tick so open
            // launcher lists refetch at the moment the change is visible.
            live.dispatch(ClientWsAction::LauncherEvent);
        }
        _ => {}
    }
}

pub(crate) fn insert_latest_session_metric(
    latest: &mut HashMap<Uuid, TurnMetrics>,
    incoming: TurnMetrics,
) {
    // An agent/version that cannot report a context window must not erase the
    // last usable gauge. The bounded trend ring still retains that raw turn.
    if incoming.context_fraction().is_none() {
        return;
    }
    let replace = latest
        .get(&incoming.session_id)
        .is_none_or(|existing| incoming.started_at >= existing.started_at);
    if replace {
        latest.insert(incoming.session_id, incoming);
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
    fn a_busy_session_does_not_evict_another_sessions_gauge() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let state = Rc::new(ClientWsState::default());

        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(a, 100))));
        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(b, 200))));
        // ...and B stays chatty.
        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(b, 300))));

        assert!(
            state.latest_session_metrics.contains_key(&a),
            "the quiet session kept its context gauge"
        );
        assert!(state.latest_session_metrics.contains_key(&b));
        assert_eq!(state.latest_session_metrics.len(), 2);
        assert_eq!(state.recent_turn_metrics.len(), 3, "trend ring accumulates");
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
        assert!(state.latest_session_metrics.contains_key(&a));

        let state = state.reduce(ClientWsAction::TurnMetrics(Box::new(gauged(b, 200))));
        assert!(
            state.latest_session_metrics.contains_key(&a),
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
        assert_eq!(latest.get(&quiet).unwrap().started_at.timestamp(), 10);
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

        assert_eq!(latest.get(&session_id).unwrap().started_at.timestamp(), 10);
    }
}

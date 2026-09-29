//! In-memory store for agent-driven progress bars (`agent-portal progress`).
//!
//! Bars are live status: they are never written to the DB, so a backend
//! restart drops them. The store keeps each session's bars so a browser that
//! connects mid-run gets the current state, and expires bars the agent stopped
//! updating so a crashed agent can't leave one hanging forever.
//!
//! Two kinds of frame leave the store, both full state with no revision, so
//! delivery order *is* state order:
//!
//! - `AgentProgress` — the session's complete bar set, to that session's web
//!   clients.
//! - `SessionProgress` — one whole-percent value for the rail pill, to every
//!   member's user channel (members are remembered from the last post, since
//!   the sweep has no DB).
//!
//! Every mutation fans both out while still holding the session's map entry,
//! and a connecting client is registered and sent its state under that same
//! entry. Two writers, or a writer and a connect, can never deliver frames out
//! of order. The lock order is always `agent_progress` entry → `web_clients` /
//! `user_clients` shard; nothing takes them reversed.

use std::time::{Duration, Instant};

use dashmap::mapref::entry::Entry;
use shared::api::{pill_fraction, ProgressBar, MAX_PROGRESS_BARS};
use shared::ServerToClient;
use uuid::Uuid;

use super::{SessionId, SessionManager, WebClientSender};

/// How long a bar survives without an update.
pub const PROGRESS_BAR_TTL: Duration = Duration::from_secs(10 * 60);

/// One session's live progress state.
#[derive(Default)]
pub(super) struct SessionProgress {
    /// Bars in creation order, each stamped with its last update.
    bars: Vec<(ProgressBar, Instant)>,
    /// Users whose rail shows this session, as of the last post.
    members: Vec<Uuid>,
    /// The whole-percent pill value last sent to `members`.
    pill_pct: Option<u8>,
}

impl SessionProgress {
    fn snapshot(&self) -> Vec<ProgressBar> {
        self.bars.iter().map(|(bar, _)| bar.clone()).collect()
    }

    fn current_pill_pct(&self) -> Option<u8> {
        let bars = self.snapshot();
        pill_fraction(&bars).map(|f| (f * 100.0).round() as u8)
    }
}

/// The bar set was full and the update named a new bar.
#[derive(Debug, PartialEq, Eq)]
pub struct ProgressBarsFull;

impl SessionManager {
    /// Create or update a bar and push the result to the session's web clients
    /// and its members' rails. `members` is the session's current member set.
    pub fn set_agent_progress(
        &self,
        session_id: Uuid,
        members: Vec<Uuid>,
        bar: ProgressBar,
    ) -> Result<(), ProgressBarsFull> {
        let mut state = self.agent_progress.entry(session_id).or_default();
        let now = Instant::now();
        match state
            .bars
            .iter()
            .position(|(existing, _)| existing.id == bar.id)
        {
            Some(i) => state.bars[i] = (bar, now),
            None if state.bars.len() >= MAX_PROGRESS_BARS => {
                // Don't leave an empty entry behind for a session that never
                // got a bar.
                let empty = state.bars.is_empty();
                drop(state);
                if empty {
                    self.agent_progress
                        .remove_if(&session_id, |_, state| state.bars.is_empty());
                }
                return Err(ProgressBarsFull);
            }
            None => state.bars.push((bar, now)),
        }
        let prev_members = std::mem::replace(&mut state.members, members);
        self.broadcast_progress(session_id, &mut state, &prev_members);
        Ok(())
    }

    /// Remove a bar and push the result, as [`Self::set_agent_progress`]. A
    /// bar that isn't there is a no-op.
    pub fn clear_agent_progress(&self, session_id: Uuid, members: Vec<Uuid>, id: &str) {
        let Entry::Occupied(mut entry) = self.agent_progress.entry(session_id) else {
            return;
        };
        let before = entry.get().bars.len();
        entry.get_mut().bars.retain(|(bar, _)| bar.id != id);
        if entry.get().bars.len() == before {
            return;
        }
        let prev_members = std::mem::replace(&mut entry.get_mut().members, members);
        self.broadcast_progress(session_id, entry.get_mut(), &prev_members);
        if entry.get().bars.is_empty() {
            entry.remove();
        }
    }

    /// Register a session-view web client and send it the current snapshot.
    ///
    /// The snapshot is always sent, empty included: a still-mounted view that
    /// reconnects after its bars expired (or the backend restarted) must be
    /// told to clear them.
    pub fn add_web_client_with_progress(&self, session_id: Uuid, sender: WebClientSender) {
        let state = self.agent_progress.entry(session_id).or_default();
        self.add_web_client(SessionId::new(session_id.to_string()), sender.clone());
        let _ = sender.send(ServerToClient::AgentProgress {
            bars: state.snapshot(),
        });
        drop(state);
        self.agent_progress
            .remove_if(&session_id, |_, state| state.bars.is_empty());
    }

    /// Register a user-wide client and send it the rail pill value of every
    /// session of theirs that has one, so a dashboard opened mid-run shows the
    /// fill without waiting for the next update.
    ///
    /// Registration comes first, then each session is read under its entry: a
    /// concurrent update is either delivered before the read (and the read
    /// sees it) or after it, so the client never ends on older state.
    pub fn add_user_client_with_progress(&self, user_id: Uuid, sender: WebClientSender) {
        self.add_user_client(user_id, sender.clone());
        let _ = sender.send(ServerToClient::SessionProgressReset);
        let sessions: Vec<Uuid> = self
            .agent_progress
            .iter()
            .filter(|entry| entry.value().members.contains(&user_id))
            .map(|entry| *entry.key())
            .collect();
        for session_id in sessions {
            let Some(state) = self.agent_progress.get(&session_id) else {
                continue;
            };
            if let Some(pct) = state.pill_pct {
                let _ = sender.send(ServerToClient::SessionProgress {
                    session_id,
                    fraction: Some(f32::from(pct) / 100.0),
                });
            }
        }
    }

    /// Drop bars not updated within `ttl` and tell each affected session's
    /// clients. Returns how many bars expired.
    pub fn sweep_stale_agent_progress(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let mut expired = 0;
        let mut emptied: Vec<Uuid> = Vec::new();
        for mut entry in self.agent_progress.iter_mut() {
            let before = entry.value().bars.len();
            entry
                .value_mut()
                .bars
                .retain(|(_, updated)| now.duration_since(*updated) < ttl);
            let dropped = before - entry.value().bars.len();
            if dropped == 0 {
                continue;
            }
            expired += dropped;
            let session_id = *entry.key();
            let members = entry.value().members.clone();
            self.broadcast_progress(session_id, entry.value_mut(), &members);
            if entry.value().bars.is_empty() {
                emptied.push(session_id);
            }
        }
        // Prune outside the iteration (removing under a shard guard can
        // deadlock), re-checking emptiness in case a writer got in first.
        for session_id in emptied {
            self.agent_progress
                .remove_if(&session_id, |_, state| state.bars.is_empty());
        }
        expired
    }

    /// Fan the session's state out: the full bar set to its web clients, and
    /// the pill value to its members. A member who was already told the current
    /// value only hears about a change; one who just joined (`prev_members`
    /// lacks them) is sent the value even if it didn't change, and one who just
    /// left is sent a clear. Callers hold the session's `agent_progress` entry
    /// (see the module docs).
    fn broadcast_progress(
        &self,
        session_id: Uuid,
        state: &mut SessionProgress,
        prev_members: &[Uuid],
    ) {
        self.broadcast_to_web_clients(
            &session_id.to_string(),
            ServerToClient::AgentProgress {
                bars: state.snapshot(),
            },
        );
        let pct = state.current_pill_pct();
        let changed = pct != state.pill_pct;
        state.pill_pct = pct;
        let frame = |pct: Option<u8>| ServerToClient::SessionProgress {
            session_id,
            fraction: pct.map(|p| f32::from(p) / 100.0),
        };
        for user_id in &state.members {
            let joined = !prev_members.contains(user_id);
            if changed || (joined && pct.is_some()) {
                self.broadcast_to_user(user_id, frame(pct));
            }
        }
        for user_id in prev_members.iter().filter(|u| !state.members.contains(u)) {
            self.broadcast_to_user(user_id, frame(None));
        }
    }

    #[cfg(test)]
    fn agent_progress_snapshot(&self, session_id: Uuid) -> Vec<ProgressBar> {
        self.agent_progress
            .get(&session_id)
            .map(|state| state.snapshot())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::websocket::conn_channel;

    fn bar(id: &str, fraction: Option<f32>) -> ProgressBar {
        ProgressBar {
            id: id.to_string(),
            label: None,
            fraction,
        }
    }

    fn drain(rx: &mut tokio::sync::mpsc::Receiver<ServerToClient>) -> Vec<Vec<ProgressBar>> {
        let mut frames = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ServerToClient::AgentProgress { bars } => frames.push(bars),
                other => panic!("expected AgentProgress, got {other:?}"),
            }
        }
        frames
    }

    #[test]
    fn set_updates_in_place_preserving_order() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        mgr.set_agent_progress(sid, vec![], bar("a", Some(0.1)))
            .unwrap();
        mgr.set_agent_progress(sid, vec![], bar("b", None)).unwrap();
        mgr.set_agent_progress(sid, vec![], bar("a", Some(0.9)))
            .unwrap();
        let snap = mgr.agent_progress_snapshot(sid);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].id, "a");
        assert_eq!(snap[0].fraction, Some(0.9));
        assert_eq!(snap[1].id, "b");
    }

    #[test]
    fn new_bar_past_the_cap_is_refused_but_existing_bars_still_update() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        for i in 0..MAX_PROGRESS_BARS {
            mgr.set_agent_progress(sid, vec![], bar(&format!("b{i}"), None))
                .unwrap();
        }
        assert_eq!(
            mgr.set_agent_progress(sid, vec![], bar("extra", None)),
            Err(ProgressBarsFull)
        );
        assert!(mgr
            .set_agent_progress(sid, vec![], bar("b0", Some(0.5)))
            .is_ok());
    }

    #[test]
    fn clear_removes_one_bar_and_forgets_an_empty_session() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        let (tx, mut rx) = conn_channel(64);
        mgr.add_web_client(sid.to_string().into(), tx);
        mgr.set_agent_progress(sid, vec![], bar("a", None)).unwrap();
        mgr.set_agent_progress(sid, vec![], bar("b", None)).unwrap();
        drain(&mut rx);

        mgr.clear_agent_progress(sid, vec![], "a");
        mgr.clear_agent_progress(sid, vec![], "b");
        let frames = drain(&mut rx);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), 1);
        assert!(frames[1].is_empty());
        assert!(mgr.agent_progress.get(&sid).is_none());

        mgr.clear_agent_progress(sid, vec![], "missing");
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn sessions_are_isolated() {
        let mgr = SessionManager::new();
        let (s1, s2) = (Uuid::new_v4(), Uuid::new_v4());
        mgr.set_agent_progress(s1, vec![], bar("a", None)).unwrap();
        assert!(mgr.agent_progress_snapshot(s2).is_empty());
        assert_eq!(mgr.agent_progress_snapshot(s1).len(), 1);
    }

    #[test]
    fn connect_always_sends_a_snapshot_even_when_empty() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();

        let (tx, mut rx) = conn_channel(64);
        mgr.add_web_client_with_progress(sid, tx);
        assert_eq!(drain(&mut rx), vec![Vec::<ProgressBar>::new()]);
        assert!(mgr.agent_progress.get(&sid).is_none());

        mgr.set_agent_progress(sid, vec![], bar("a", Some(0.5)))
            .unwrap();
        let (tx2, mut rx2) = conn_channel(64);
        mgr.add_web_client_with_progress(sid, tx2);
        assert_eq!(drain(&mut rx2), vec![vec![bar("a", Some(0.5))]]);
    }

    #[test]
    fn sweep_expires_only_stale_bars_and_notifies_clients() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        mgr.set_agent_progress(sid, vec![], bar("old", None))
            .unwrap();
        let (tx, mut rx) = conn_channel(64);
        mgr.add_web_client(sid.to_string().into(), tx);

        assert_eq!(mgr.sweep_stale_agent_progress(Duration::from_secs(60)), 0);
        assert!(drain(&mut rx).is_empty());

        assert_eq!(mgr.sweep_stale_agent_progress(Duration::ZERO), 1);
        assert_eq!(drain(&mut rx), vec![Vec::<ProgressBar>::new()]);
        assert!(mgr.agent_progress.get(&sid).is_none());
    }

    /// Concurrent setters and clearers must deliver snapshots in state order:
    /// the last frame a client sees is the store's final state, and no frame
    /// after a clear resurrects the cleared bar.
    #[test]
    fn concurrent_writers_deliver_snapshots_in_state_order() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        let (tx, mut rx) = conn_channel(1 << 16);
        mgr.add_web_client(sid.to_string().into(), tx);

        std::thread::scope(|scope| {
            for t in 0..6u32 {
                let mgr = &mgr;
                scope.spawn(move || {
                    for i in 0..200u32 {
                        let id = format!("b{}", (t + i) % 3);
                        if i % 4 == 3 {
                            mgr.clear_agent_progress(sid, vec![], &id);
                        } else {
                            mgr.set_agent_progress(
                                sid,
                                vec![],
                                bar(&id, Some((i % 100) as f32 / 100.0)),
                            )
                            .unwrap();
                        }
                    }
                });
            }
        });

        let frames = drain(&mut rx);
        assert_eq!(
            frames.last().cloned().unwrap_or_default(),
            mgr.agent_progress_snapshot(sid),
            "last delivered snapshot must equal the final store state"
        );
    }

    /// A client connecting while writers run must end up consistent with the
    /// store: its connect snapshot plus later frames never end on stale state.
    #[test]
    fn connect_racing_writers_ends_consistent() {
        for _ in 0..50 {
            let mgr = SessionManager::new();
            let sid = Uuid::new_v4();
            let (tx, mut rx) = conn_channel(1 << 12);

            std::thread::scope(|scope| {
                let mgr = &mgr;
                scope.spawn(move || {
                    for i in 0..50u32 {
                        mgr.set_agent_progress(sid, vec![], bar("a", Some(i as f32 / 100.0)))
                            .unwrap();
                    }
                });
                scope.spawn(move || mgr.add_web_client_with_progress(sid, tx));
            });

            let frames = drain(&mut rx);
            assert!(!frames.is_empty(), "connect must deliver a snapshot");
            assert_eq!(
                frames.last().cloned().unwrap(),
                mgr.agent_progress_snapshot(sid)
            );
        }
    }

    fn drain_pill(
        rx: &mut tokio::sync::mpsc::Receiver<ServerToClient>,
    ) -> Vec<(Uuid, Option<f32>)> {
        let mut frames = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ServerToClient::SessionProgress {
                    session_id,
                    fraction,
                } => frames.push((session_id, fraction)),
                other => panic!("expected SessionProgress, got {other:?}"),
            }
        }
        frames
    }

    #[test]
    fn members_get_a_pill_frame_only_when_the_whole_percent_changes() {
        let mgr = SessionManager::new();
        let (sid, user) = (Uuid::new_v4(), Uuid::new_v4());
        let (tx, mut rx) = conn_channel(64);
        mgr.add_user_client(user, tx);

        mgr.set_agent_progress(sid, vec![user], bar("a", Some(0.501)))
            .unwrap();
        mgr.set_agent_progress(sid, vec![user], bar("a", Some(0.502)))
            .unwrap();
        mgr.set_agent_progress(sid, vec![user], bar("a", Some(0.75)))
            .unwrap();
        assert_eq!(
            drain_pill(&mut rx),
            vec![(sid, Some(0.5)), (sid, Some(0.75))]
        );

        mgr.clear_agent_progress(sid, vec![user], "a");
        assert_eq!(drain_pill(&mut rx), vec![(sid, None)]);
    }

    #[test]
    fn membership_changes_reach_added_and_removed_users_at_an_unchanged_percent() {
        let mgr = SessionManager::new();
        let (sid, old, new) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (tx_old, mut rx_old) = conn_channel(64);
        let (tx_new, mut rx_new) = conn_channel(64);
        mgr.add_user_client(old, tx_old);
        mgr.add_user_client(new, tx_new);

        mgr.set_agent_progress(sid, vec![old], bar("a", Some(0.5)))
            .unwrap();
        assert_eq!(drain_pill(&mut rx_old), vec![(sid, Some(0.5))]);

        // Same percent, but the member set swapped.
        mgr.set_agent_progress(sid, vec![new], bar("a", Some(0.5)))
            .unwrap();
        assert_eq!(drain_pill(&mut rx_new), vec![(sid, Some(0.5))]);
        assert_eq!(drain_pill(&mut rx_old), vec![(sid, None)]);
    }

    #[test]
    fn user_connect_starts_with_a_reset_even_when_nothing_is_running() {
        let mgr = SessionManager::new();
        let (tx, mut rx) = conn_channel(64);
        mgr.add_user_client_with_progress(Uuid::new_v4(), tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerToClient::SessionProgressReset)
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn indeterminate_only_bars_send_no_pill_frame() {
        let mgr = SessionManager::new();
        let (sid, user) = (Uuid::new_v4(), Uuid::new_v4());
        let (tx, mut rx) = conn_channel(64);
        mgr.add_user_client(user, tx);
        mgr.set_agent_progress(sid, vec![user], bar("a", None))
            .unwrap();
        assert!(drain_pill(&mut rx).is_empty());
    }

    #[test]
    fn pill_frames_reach_members_only() {
        let mgr = SessionManager::new();
        let (sid, member, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (tx_m, mut rx_m) = conn_channel(64);
        let (tx_o, mut rx_o) = conn_channel(64);
        mgr.add_user_client(member, tx_m);
        mgr.add_user_client(other, tx_o);
        mgr.set_agent_progress(sid, vec![member], bar("a", Some(0.5)))
            .unwrap();
        assert_eq!(drain_pill(&mut rx_m).len(), 1);
        assert!(drain_pill(&mut rx_o).is_empty());
    }

    #[test]
    fn user_connect_replays_pill_for_member_sessions_only() {
        let mgr = SessionManager::new();
        let (mine, theirs, user, other) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        mgr.set_agent_progress(mine, vec![user], bar("a", Some(0.25)))
            .unwrap();
        mgr.set_agent_progress(theirs, vec![other], bar("a", Some(0.9)))
            .unwrap();

        let (tx, mut rx) = conn_channel(64);
        mgr.add_user_client_with_progress(user, tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerToClient::SessionProgressReset)
        ));
        assert_eq!(drain_pill(&mut rx), vec![(mine, Some(0.25))]);
    }

    #[test]
    fn sweep_clears_the_pill_for_members() {
        let mgr = SessionManager::new();
        let (sid, user) = (Uuid::new_v4(), Uuid::new_v4());
        let (tx, mut rx) = conn_channel(64);
        mgr.add_user_client(user, tx);
        mgr.set_agent_progress(sid, vec![user], bar("a", Some(0.5)))
            .unwrap();
        drain_pill(&mut rx);

        mgr.sweep_stale_agent_progress(Duration::ZERO);
        assert_eq!(drain_pill(&mut rx), vec![(sid, None)]);
    }
}

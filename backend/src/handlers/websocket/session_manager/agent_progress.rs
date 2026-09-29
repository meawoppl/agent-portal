//! In-memory store for agent-driven progress bars (`agent-portal progress`).
//!
//! Bars are live status: they are never written to the DB, so a backend
//! restart drops them. The store keeps each session's bars so a browser that
//! connects mid-run gets the current state, and expires bars the agent stopped
//! updating so a crashed agent can't leave one hanging forever.
//!
//! Clients receive full snapshots with no revision, so delivery order *is*
//! state order. Every mutation therefore fans its snapshot out while still
//! holding the session's map entry, and a connecting client is registered and
//! sent its snapshot under that same entry. Two writers, or a writer and a
//! connect, can never deliver snapshots out of order. The lock order is always
//! `agent_progress` entry → `web_clients` shard; nothing takes them reversed.

use std::time::{Duration, Instant};

use dashmap::mapref::entry::Entry;
use shared::api::{ProgressBar, MAX_PROGRESS_BARS};
use shared::ServerToClient;
use uuid::Uuid;

use super::{SessionId, SessionManager, WebClientSender};

/// How long a bar survives without an update.
pub const PROGRESS_BAR_TTL: Duration = Duration::from_secs(10 * 60);

/// A session's bars in creation order, each stamped with its last update.
pub(super) type ProgressBars = Vec<(ProgressBar, Instant)>;

/// The bar set was full and the update named a new bar.
#[derive(Debug, PartialEq, Eq)]
pub struct ProgressBarsFull;

impl SessionManager {
    /// Create or update a bar and push the session's resulting snapshot to its
    /// web clients.
    pub fn set_agent_progress(
        &self,
        session_id: Uuid,
        bar: ProgressBar,
    ) -> Result<(), ProgressBarsFull> {
        let mut bars = self.agent_progress.entry(session_id).or_default();
        let now = Instant::now();
        match bars.iter().position(|(existing, _)| existing.id == bar.id) {
            Some(i) => bars[i] = (bar, now),
            None if bars.len() >= MAX_PROGRESS_BARS => {
                // Don't leave an empty entry behind for a session that never
                // got a bar.
                let empty = bars.is_empty();
                drop(bars);
                if empty {
                    self.agent_progress
                        .remove_if(&session_id, |_, bars| bars.is_empty());
                }
                return Err(ProgressBarsFull);
            }
            None => bars.push((bar, now)),
        }
        self.broadcast_progress(session_id, &bars);
        Ok(())
    }

    /// Remove a bar and push the session's resulting snapshot to its web
    /// clients. A bar that isn't there is a no-op.
    pub fn clear_agent_progress(&self, session_id: Uuid, id: &str) {
        let Entry::Occupied(mut entry) = self.agent_progress.entry(session_id) else {
            return;
        };
        let before = entry.get().len();
        entry.get_mut().retain(|(bar, _)| bar.id != id);
        if entry.get().len() == before {
            return;
        }
        self.broadcast_progress(session_id, entry.get());
        if entry.get().is_empty() {
            entry.remove();
        }
    }

    /// Register a web client for a session and send it the current snapshot.
    ///
    /// The snapshot is always sent, empty included: a still-mounted view that
    /// reconnects after its bars expired (or the backend restarted) must be
    /// told to clear them.
    pub fn add_web_client_with_progress(&self, session_id: Uuid, sender: WebClientSender) {
        let bars = self.agent_progress.entry(session_id).or_default();
        self.add_web_client(SessionId::new(session_id.to_string()), sender.clone());
        let _ = sender.send(ServerToClient::AgentProgress {
            bars: snapshot(&bars),
        });
        drop(bars);
        self.agent_progress
            .remove_if(&session_id, |_, bars| bars.is_empty());
    }

    /// Drop bars not updated within `ttl` and tell each affected session's web
    /// clients. Returns how many bars expired.
    pub fn sweep_stale_agent_progress(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let mut expired = 0;
        let mut emptied: Vec<Uuid> = Vec::new();
        for mut entry in self.agent_progress.iter_mut() {
            let before = entry.value().len();
            entry
                .value_mut()
                .retain(|(_, updated)| now.duration_since(*updated) < ttl);
            let dropped = before - entry.value().len();
            if dropped == 0 {
                continue;
            }
            expired += dropped;
            self.broadcast_progress(*entry.key(), entry.value());
            if entry.value().is_empty() {
                emptied.push(*entry.key());
            }
        }
        // Prune outside the iteration (removing under a shard guard can
        // deadlock), re-checking emptiness in case a writer got in first.
        for session_id in emptied {
            self.agent_progress
                .remove_if(&session_id, |_, bars| bars.is_empty());
        }
        expired
    }

    /// Fan `bars` out to the session's web clients. Callers hold the session's
    /// `agent_progress` entry (see the module docs).
    fn broadcast_progress(&self, session_id: Uuid, bars: &ProgressBars) {
        self.broadcast_to_web_clients(
            &session_id.to_string(),
            ServerToClient::AgentProgress {
                bars: snapshot(bars),
            },
        );
    }

    #[cfg(test)]
    fn agent_progress_snapshot(&self, session_id: Uuid) -> Vec<ProgressBar> {
        self.agent_progress
            .get(&session_id)
            .map(|bars| snapshot(&bars))
            .unwrap_or_default()
    }
}

fn snapshot(bars: &ProgressBars) -> Vec<ProgressBar> {
    bars.iter().map(|(bar, _)| bar.clone()).collect()
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
        mgr.set_agent_progress(sid, bar("a", Some(0.1))).unwrap();
        mgr.set_agent_progress(sid, bar("b", None)).unwrap();
        mgr.set_agent_progress(sid, bar("a", Some(0.9))).unwrap();
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
            mgr.set_agent_progress(sid, bar(&format!("b{i}"), None))
                .unwrap();
        }
        assert_eq!(
            mgr.set_agent_progress(sid, bar("extra", None)),
            Err(ProgressBarsFull)
        );
        assert!(mgr.set_agent_progress(sid, bar("b0", Some(0.5))).is_ok());
    }

    #[test]
    fn clear_removes_one_bar_and_forgets_an_empty_session() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        let (tx, mut rx) = conn_channel(64);
        mgr.add_web_client(sid.to_string().into(), tx);
        mgr.set_agent_progress(sid, bar("a", None)).unwrap();
        mgr.set_agent_progress(sid, bar("b", None)).unwrap();
        drain(&mut rx);

        mgr.clear_agent_progress(sid, "a");
        mgr.clear_agent_progress(sid, "b");
        let frames = drain(&mut rx);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), 1);
        assert!(frames[1].is_empty());
        assert!(mgr.agent_progress.get(&sid).is_none());

        mgr.clear_agent_progress(sid, "missing");
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn sessions_are_isolated() {
        let mgr = SessionManager::new();
        let (s1, s2) = (Uuid::new_v4(), Uuid::new_v4());
        mgr.set_agent_progress(s1, bar("a", None)).unwrap();
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

        mgr.set_agent_progress(sid, bar("a", Some(0.5))).unwrap();
        let (tx2, mut rx2) = conn_channel(64);
        mgr.add_web_client_with_progress(sid, tx2);
        assert_eq!(drain(&mut rx2), vec![vec![bar("a", Some(0.5))]]);
    }

    #[test]
    fn sweep_expires_only_stale_bars_and_notifies_clients() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        mgr.set_agent_progress(sid, bar("old", None)).unwrap();
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
                            mgr.clear_agent_progress(sid, &id);
                        } else {
                            mgr.set_agent_progress(sid, bar(&id, Some((i % 100) as f32 / 100.0)))
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
                        mgr.set_agent_progress(sid, bar("a", Some(i as f32 / 100.0)))
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
}

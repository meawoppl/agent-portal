//! In-memory store for agent-driven progress bars (`agent-portal progress`).
//!
//! Bars are live status: they are never written to the DB, so a backend
//! restart drops them. The store keeps each session's bars so a browser that
//! connects mid-run gets the current state, and expires bars the agent stopped
//! updating so a crashed agent can't leave one hanging forever.

use std::time::{Duration, Instant};

use shared::api::{ProgressBar, MAX_PROGRESS_BARS};
use shared::ServerToClient;
use uuid::Uuid;

use super::SessionManager;

/// How long a bar survives without an update.
pub const PROGRESS_BAR_TTL: Duration = Duration::from_secs(10 * 60);

/// A session's bars in creation order, each stamped with its last update.
pub(super) type ProgressBars = Vec<(ProgressBar, Instant)>;

/// The bar set was full and the update named a new bar.
#[derive(Debug, PartialEq, Eq)]
pub struct ProgressBarsFull;

impl SessionManager {
    /// Create or update a bar and return the session's resulting snapshot.
    pub fn set_agent_progress(
        &self,
        session_id: Uuid,
        bar: ProgressBar,
    ) -> Result<Vec<ProgressBar>, ProgressBarsFull> {
        let mut bars = self.agent_progress.entry(session_id).or_default();
        let now = Instant::now();
        match bars.iter().position(|(existing, _)| existing.id == bar.id) {
            Some(i) => bars[i] = (bar, now),
            None if bars.len() >= MAX_PROGRESS_BARS => return Err(ProgressBarsFull),
            None => bars.push((bar, now)),
        }
        Ok(snapshot(&bars))
    }

    /// Remove a bar and return the session's resulting snapshot.
    pub fn clear_agent_progress(&self, session_id: Uuid, id: &str) -> Vec<ProgressBar> {
        let Some(mut bars) = self.agent_progress.get_mut(&session_id) else {
            return Vec::new();
        };
        bars.retain(|(bar, _)| bar.id != id);
        let remaining = snapshot(&bars);
        drop(bars);
        if remaining.is_empty() {
            self.agent_progress.remove(&session_id);
        }
        remaining
    }

    /// The session's current bars, oldest first. Empty when none are live.
    pub fn agent_progress_snapshot(&self, session_id: Uuid) -> Vec<ProgressBar> {
        self.agent_progress
            .get(&session_id)
            .map(|bars| snapshot(&bars))
            .unwrap_or_default()
    }

    /// Drop bars not updated within `ttl` and tell each affected session's web
    /// clients. Returns how many bars expired.
    pub fn sweep_stale_agent_progress(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let mut expired = 0;
        let mut changed: Vec<(Uuid, Vec<ProgressBar>)> = Vec::new();
        for mut entry in self.agent_progress.iter_mut() {
            let before = entry.value().len();
            entry
                .value_mut()
                .retain(|(_, updated)| now.duration_since(*updated) < ttl);
            let dropped = before - entry.value().len();
            if dropped > 0 {
                expired += dropped;
                changed.push((*entry.key(), snapshot(entry.value())));
            }
        }
        // Broadcast and prune outside the iteration: mutating the map while a
        // shard guard is held can deadlock.
        for (session_id, bars) in changed {
            if bars.is_empty() {
                self.agent_progress
                    .remove_if(&session_id, |_, bars| bars.is_empty());
            }
            self.broadcast_to_web_clients(
                &session_id.to_string(),
                ServerToClient::AgentProgress { bars },
            );
        }
        expired
    }
}

fn snapshot(bars: &ProgressBars) -> Vec<ProgressBar> {
    bars.iter().map(|(bar, _)| bar.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(id: &str, fraction: Option<f32>) -> ProgressBar {
        ProgressBar {
            id: id.to_string(),
            label: None,
            fraction,
        }
    }

    #[test]
    fn set_updates_in_place_preserving_order() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        mgr.set_agent_progress(sid, bar("a", Some(0.1))).unwrap();
        mgr.set_agent_progress(sid, bar("b", None)).unwrap();
        let snap = mgr.set_agent_progress(sid, bar("a", Some(0.9))).unwrap();
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
        mgr.set_agent_progress(sid, bar("a", None)).unwrap();
        mgr.set_agent_progress(sid, bar("b", None)).unwrap();
        assert_eq!(mgr.clear_agent_progress(sid, "a").len(), 1);
        assert!(mgr.clear_agent_progress(sid, "b").is_empty());
        assert!(mgr.agent_progress.get(&sid).is_none());
        assert!(mgr.clear_agent_progress(sid, "missing").is_empty());
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
    fn sweep_expires_only_stale_bars_and_notifies_clients() {
        let mgr = SessionManager::new();
        let sid = Uuid::new_v4();
        mgr.set_agent_progress(sid, bar("old", None)).unwrap();
        let (tx, mut rx) = crate::handlers::websocket::conn_channel(64);
        mgr.add_web_client(sid.to_string().into(), tx);

        assert_eq!(mgr.sweep_stale_agent_progress(Duration::from_secs(60)), 0);
        assert!(rx.try_recv().is_err());

        assert_eq!(mgr.sweep_stale_agent_progress(Duration::ZERO), 1);
        match rx.try_recv() {
            Ok(ServerToClient::AgentProgress { bars }) => assert!(bars.is_empty()),
            other => panic!("expected AgentProgress, got {other:?}"),
        }
        assert!(mgr.agent_progress.get(&sid).is_none());
    }
}

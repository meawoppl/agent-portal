//! Server-side input idempotency by `client_msg_id` (#1236).
//!
//! The web client's send outbox (#1235) resends unacked inputs after a
//! reconnect. Resending anything that *might* have reached the backend
//! risks a duplicate reaching the agent, so the outbox used to resend only
//! frames never handed to the transport — leaving an in-flight-loss window
//! (frame accepted by the socket queue, socket dies before the write).
//!
//! This module closes it from the server side: the backend remembers the
//! recent `client_msg_id`s per session and their delivery state, so the
//! client can resend *everything* unacked (at-least-once) and the backend
//! collapses duplicates (idempotent), re-emitting the terminal stage so the
//! client's outbox still resolves.
//!
//! Bounds: ids per session are capped (resends happen within seconds of the
//! original; the cap is generous). The per-session entry survives proxy
//! reconnects deliberately — it must, since replayed inputs carry the same
//! ids. Memory: ≤ `MAX_TRACKED_IDS_PER_SESSION` small tuples per live
//! session. A backend restart clears the map; the durable `pending_inputs`
//! rows (which now persist `client_msg_id`) backstop the in-flight case,
//! checked by the caller in `web_client_socket`.

use std::collections::VecDeque;

use uuid::Uuid;

use super::SessionManager;

/// Cap on remembered ids per session. Resends arrive within seconds of the
/// original send; 256 covers a pathological burst without meaningful memory.
const MAX_TRACKED_IDS_PER_SESSION: usize = 256;

/// Delivery state of a tracked input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputDeliveryState {
    /// Enqueued toward the agent; no terminal ack from the proxy yet.
    InFlight,
    /// The agent process accepted the input (proxy reported `AgentAccepted`).
    Accepted,
    /// The proxy reported delivery failure.
    Failed,
}

/// Outcome of checking an incoming `client_msg_id` against the tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DedupVerdict {
    /// Never seen: recorded as in-flight; caller proceeds with delivery.
    New,
    /// Already tracked with the given state; caller must NOT re-deliver.
    Duplicate(InputDeliveryState),
}

impl SessionManager {
    /// Check an incoming input's `client_msg_id`; if new, record it as
    /// in-flight. One entry-lock operation, so two concurrent duplicates
    /// can't both see `New`.
    pub(crate) fn check_and_record_input(
        &self,
        session_id: Uuid,
        client_msg_id: Uuid,
    ) -> DedupVerdict {
        let mut tracked = self.input_dedup.entry(session_id).or_default();
        if let Some((_, state)) = tracked.iter().find(|(id, _)| *id == client_msg_id) {
            return DedupVerdict::Duplicate(*state);
        }
        if tracked.len() >= MAX_TRACKED_IDS_PER_SESSION {
            tracked.pop_front();
        }
        tracked.push_back((client_msg_id, InputDeliveryState::InFlight));
        DedupVerdict::New
    }

    /// Record a terminal delivery state reported by the proxy. Inserts the
    /// id if untracked (e.g. the backend restarted between enqueue and ack)
    /// so a later resend still re-acks instead of re-delivering.
    pub(crate) fn record_input_terminal(
        &self,
        session_id: Uuid,
        client_msg_id: Uuid,
        state: InputDeliveryState,
    ) {
        let mut tracked = self.input_dedup.entry(session_id).or_default();
        match tracked.iter_mut().find(|(id, _)| *id == client_msg_id) {
            Some(entry) => entry.1 = state,
            None => {
                if tracked.len() >= MAX_TRACKED_IDS_PER_SESSION {
                    tracked.pop_front();
                }
                tracked.push_back((client_msg_id, state));
            }
        }
    }
}

pub(super) type InputDedupQueue = VecDeque<(Uuid, InputDeliveryState)>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_in_flight_and_terminal_transitions() {
        let mgr = SessionManager::new();
        let session = Uuid::new_v4();
        let id = Uuid::new_v4();

        assert_eq!(mgr.check_and_record_input(session, id), DedupVerdict::New);
        assert_eq!(
            mgr.check_and_record_input(session, id),
            DedupVerdict::Duplicate(InputDeliveryState::InFlight)
        );

        mgr.record_input_terminal(session, id, InputDeliveryState::Accepted);
        assert_eq!(
            mgr.check_and_record_input(session, id),
            DedupVerdict::Duplicate(InputDeliveryState::Accepted)
        );
    }

    #[test]
    fn terminal_for_untracked_id_is_recorded() {
        let mgr = SessionManager::new();
        let session = Uuid::new_v4();
        let id = Uuid::new_v4();

        // Simulates a backend restart between enqueue and the proxy's ack.
        mgr.record_input_terminal(session, id, InputDeliveryState::Failed);
        assert_eq!(
            mgr.check_and_record_input(session, id),
            DedupVerdict::Duplicate(InputDeliveryState::Failed)
        );
    }

    #[test]
    fn tracker_is_bounded_per_session() {
        let mgr = SessionManager::new();
        let session = Uuid::new_v4();
        let first = Uuid::new_v4();
        assert_eq!(
            mgr.check_and_record_input(session, first),
            DedupVerdict::New
        );

        for _ in 0..MAX_TRACKED_IDS_PER_SESSION {
            mgr.check_and_record_input(session, Uuid::new_v4());
        }

        // The oldest id fell off the window: it reads as new again. This is
        // the accepted bound — resends arrive within seconds, not after 256
        // intervening sends.
        assert_eq!(
            mgr.check_and_record_input(session, first),
            DedupVerdict::New
        );
        assert!(
            mgr.input_dedup.get(&session).unwrap().len() <= MAX_TRACKED_IDS_PER_SESSION,
            "tracker must stay bounded"
        );
    }

    #[test]
    fn sessions_are_tracked_independently() {
        let mgr = SessionManager::new();
        let id = Uuid::new_v4();
        assert_eq!(
            mgr.check_and_record_input(Uuid::new_v4(), id),
            DedupVerdict::New
        );
        assert_eq!(
            mgr.check_and_record_input(Uuid::new_v4(), id),
            DedupVerdict::New
        );
    }
}

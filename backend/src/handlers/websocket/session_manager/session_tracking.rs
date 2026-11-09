//! Narrow accessors for per-session *tracking* state — the output-ack
//! watermark, the last input sender, and the sub-agent token rollup.
//!
//! These were public `DashMap` fields that handlers poked directly; routing
//! them through intent-named methods (and making the fields private) is part of
//! the SessionManager de-god (#1165 item 4). Behavior is unchanged — each
//! method is the exact storage operation its former call site performed.

use uuid::Uuid;

use super::SessionManager;

impl SessionManager {
    /// Permanent deletion only: ordinary disconnects must retain ack and
    /// dedup watermarks so reconnects cannot duplicate inputs or output.
    pub fn forget_deleted_session(&self, session_id: Uuid, session_key: &str) {
        if let Some((_, connection)) = self.sessions.remove(session_key) {
            connection.cancel.cancel();
            self.close_tunnels_for_connection(session_key, connection.gen);
        }
        if let Some((_, plane)) = self.data_planes.remove(session_key) {
            plane.cancel.cancel();
        }
        self.pending_messages.remove(session_key);
        self.last_ack_seq.remove(&session_id);
        self.last_input_sender.remove(&session_id);
        self.subagent_tokens.remove(&session_id);
        self.input_dedup.remove(&session_id);
        self.pending_truncations.remove(&session_id);
        self.active_turns.remove(&session_id);
        self.edit_stack_compaction.remove(&session_id);
        self.pending_launch_sessions
            .retain(|_, id| *id != session_id);
        self.pending_forward_status
            .retain(|(id, _), _| *id != session_id);
        self.forget_forward_health_for_session(session_id);
        self.forward_failures.remove(&session_id);
        self.forget_session_progress(session_id);
        self.web_clients.remove(session_id.to_string().as_str());
    }

    /// Highest output sequence number acked for `session_id` (0 if none yet).
    /// Used to deduplicate already-broadcast sequenced output.
    pub(crate) fn last_ack_seq(&self, session_id: Uuid) -> u64 {
        self.last_ack_seq.get(&session_id).map(|v| *v).unwrap_or(0)
    }

    /// Advance the per-session ack watermark to `seq` when it is newer.
    pub(crate) fn record_ack_seq(&self, session_id: Uuid, seq: u64) {
        self.last_ack_seq
            .entry(session_id)
            .and_modify(|v| {
                if seq > *v {
                    *v = seq;
                }
            })
            .or_insert(seq);
    }

    /// Record who sent the most recent input for `session_id` (for sender
    /// attribution on the echoed user message).
    pub(crate) fn set_last_input_sender(&self, session_id: Uuid, user_id: Uuid, name: String) {
        self.last_input_sender.insert(session_id, (user_id, name));
    }

    /// Remove and return the last input sender for `session_id`, if any. The
    /// echoed user message consumes it exactly once.
    pub(crate) fn take_last_input_sender(&self, session_id: Uuid) -> Option<(Uuid, String)> {
        self.last_input_sender.remove(&session_id).map(|(_, v)| v)
    }

    /// Add `tokens` to the running sub-agent (Task tool) token total for
    /// `session_id`; folded into the session's `output_tokens` at result time.
    pub(crate) fn add_subagent_tokens(&self, session_id: Uuid, tokens: i64) {
        self.subagent_tokens
            .entry(session_id)
            .and_modify(|v| *v += tokens)
            .or_insert(tokens);
    }

    /// The accumulated sub-agent token total for `session_id` (0 if none).
    pub(crate) fn subagent_tokens(&self, session_id: Uuid) -> i64 {
        self.subagent_tokens
            .get(&session_id)
            .map(|v| *v)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{conn_channel, DedupVerdict, InputDeliveryState};
    use super::*;
    use tokio_util::sync::CancellationToken;
    #[test]
    fn permanent_deletion_clears_state_but_reconnect_preserves_dedup() {
        let mgr = SessionManager::new();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let input = Uuid::new_v4();
        for id in [a, b] {
            mgr.record_ack_seq(id, 7);
            mgr.add_subagent_tokens(id, 99);
            mgr.check_and_record_input(id, input);
            mgr.update_forward_health(id, 8080, true, None);
        }
        let (tx, _rx) = conn_channel(4);
        let gen = mgr.register_session(a.to_string().into(), tx, CancellationToken::new());
        mgr.unregister_session(&a.to_string(), Some(gen));
        assert_eq!(
            mgr.check_and_record_input(a, input),
            DedupVerdict::Duplicate(InputDeliveryState::InFlight)
        );
        mgr.forget_deleted_session(a, &a.to_string());
        assert_eq!(mgr.last_ack_seq(a), 0);
        assert_eq!(mgr.subagent_tokens(a), 0);
        assert!(!mgr.input_dedup.contains_key(&a));
        assert_eq!(mgr.forward_health(a, 8080), None);
        assert_eq!(mgr.last_ack_seq(b), 7);
        assert_eq!(mgr.subagent_tokens(b), 99);
        assert!(mgr.input_dedup.contains_key(&b));
    }
}

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

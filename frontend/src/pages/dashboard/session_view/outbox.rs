//! Client-side send outbox for reliable web-client input delivery.
//!
//! `AgentInput` frames are sent fire-and-forget over the WebSocket. When the
//! socket is down — `ws_sender` is cleared to `None` in `handle_ws_error`, and
//! a send can also fail if the transport channel is closing — the frame is
//! silently dropped: the optimistic echo renders but nothing reaches the
//! backend and nothing ever resends it. That is the "messages deliver somewhat
//! unreliably" bug.
//!
//! The outbox closes that hole:
//! - Every `AgentInput` is recorded here, keyed by its `client_msg_id`, with a
//!   `transmitted` flag: was the frame actually handed to the transport?
//! - On reconnect the caller flushes only the **un-transmitted** frames.
//!   Because those provably never reached the backend, resending them cannot
//!   duplicate — so no backend dedupe is required.
//! - An entry is removed once the backend resolves it: `AgentAccepted`
//!   (delivered) or `Failed` (terminal).
//! - The outbox is bounded; the oldest entry is evicted past the cap so a
//!   perpetually-unacked backlog can't grow without limit.
//!
//! NOT covered here (would require backend idempotency keyed on
//! `client_msg_id`, tracked as a follow-up): a frame handed to a socket that
//! dies before it transmits. Such a frame is marked `transmitted` and
//! deliberately not resent, keeping the outbox strictly dup-free.

use shared::ClientToServer;
use uuid::Uuid;

/// Max entries retained before the oldest is evicted.
pub(super) const MAX_OUTBOX_ENTRIES: usize = 50;

struct OutboxEntry {
    client_msg_id: Uuid,
    frame: ClientToServer,
    transmitted: bool,
}

/// FIFO of in-flight `AgentInput` frames awaiting backend confirmation.
#[derive(Default)]
pub(super) struct Outbox {
    entries: Vec<OutboxEntry>,
}

impl Outbox {
    /// Record a freshly-built input frame (not yet transmitted). Returns the
    /// `client_msg_id`s of any entries evicted to stay within the cap, so the
    /// caller can surface them as failed.
    pub(super) fn record(&mut self, client_msg_id: Uuid, frame: ClientToServer) -> Vec<Uuid> {
        self.entries.push(OutboxEntry {
            client_msg_id,
            frame,
            transmitted: false,
        });
        let mut dropped = Vec::new();
        while self.entries.len() > MAX_OUTBOX_ENTRIES {
            dropped.push(self.entries.remove(0).client_msg_id);
        }
        dropped
    }

    /// Mark an entry as handed to the transport (a successful WS send).
    pub(super) fn mark_transmitted(&mut self, client_msg_id: Uuid) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.client_msg_id == client_msg_id)
        {
            entry.transmitted = true;
        }
    }

    /// Remove an entry once its delivery is resolved (accepted or failed).
    /// Returns whether an entry was actually present.
    pub(super) fn resolve(&mut self, client_msg_id: Uuid) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.client_msg_id != client_msg_id);
        self.entries.len() != before
    }

    /// `(client_msg_id, frame)` clones for every entry never handed to the
    /// transport, in send order. The caller sends each and calls
    /// [`mark_transmitted`](Self::mark_transmitted) on success — so a flush
    /// that itself fails leaves the entry queued for the next reconnect.
    pub(super) fn untransmitted(&self) -> Vec<(Uuid, ClientToServer)> {
        self.entries
            .iter()
            .filter(|e| !e.transmitted)
            .map(|e| (e.client_msg_id, e.frame.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tag: &str) -> ClientToServer {
        ClientToServer::AgentInput {
            content: serde_json::Value::String(tag.to_string()),
            send_mode: None,
            client_msg_id: None,
        }
    }

    #[test]
    fn untransmitted_lists_only_unsent_in_order() {
        let mut ob = Outbox::default();
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        ob.record(a, input("a"));
        ob.record(b, input("b"));
        ob.mark_transmitted(a);

        let pending: Vec<Uuid> = ob.untransmitted().into_iter().map(|(id, _)| id).collect();
        assert_eq!(pending, vec![b], "only the un-transmitted frame flushes");
    }

    #[test]
    fn flush_is_dup_free_after_marking_transmitted() {
        // The reconnect flush must not re-send a frame it already sent — this
        // is what keeps resends free of duplicates without backend dedupe.
        let mut ob = Outbox::default();
        let a = Uuid::from_u128(1);
        ob.record(a, input("a"));

        // First flush: send + mark.
        for (id, _frame) in ob.untransmitted() {
            ob.mark_transmitted(id);
        }
        // Second flush (e.g. a later reconnect) returns nothing.
        assert!(
            ob.untransmitted().is_empty(),
            "a transmitted frame is never re-sent"
        );
    }

    #[test]
    fn resolve_removes_entry() {
        let mut ob = Outbox::default();
        let a = Uuid::from_u128(1);
        ob.record(a, input("a"));
        assert_eq!(ob.len(), 1);
        assert!(ob.resolve(a), "known id resolves");
        assert_eq!(ob.len(), 0);
        assert!(!ob.resolve(a), "unknown id is a no-op");
    }

    #[test]
    fn cap_evicts_oldest_and_reports_it() {
        let mut ob = Outbox::default();
        for i in 0..MAX_OUTBOX_ENTRIES {
            let dropped = ob.record(Uuid::from_u128(i as u128), input("x"));
            assert!(dropped.is_empty());
        }
        // One past the cap evicts the oldest (id 0).
        let dropped = ob.record(Uuid::from_u128(999), input("overflow"));
        assert_eq!(dropped, vec![Uuid::from_u128(0)]);
        assert_eq!(ob.len(), MAX_OUTBOX_ENTRIES);
    }
}

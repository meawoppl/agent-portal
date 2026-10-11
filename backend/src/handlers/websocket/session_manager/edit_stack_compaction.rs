//! Compact-before-dispatch bookkeeping for the edit-stack work queue.
//!
//! Before promoting the next queued item into a Claude session whose context
//! is already at least half full, the dispatcher sends `/compact` and holds the
//! queue until the compaction settles. A Claude compaction produces two
//! terminal results: the `/compact` turn itself, then the portal reminder the
//! proxy injects after every compaction boundary (see
//! `session-lib/src/proxy_session/portal_reminder.rs`). The backend never sees
//! that reminder as an input, so this per-session phase is what keeps the next
//! item from racing it.
//!
//! The state is in memory only: a backend restart drops it, and the worst case
//! is one item dispatched without a preceding compaction.

use std::time::{Duration, Instant};

use uuid::Uuid;

use super::SessionManager;

/// A held phase older than this is treated as abandoned (proxy gone mid
/// compaction, a lost result) so the queue cannot stall forever.
pub(crate) const COMPACTION_HOLD_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionPhase {
    /// `/compact` was sent; waiting for its boundary and result.
    Requested,
    /// The compaction boundary arrived; the `/compact` result is still due.
    Compacted,
    /// The `/compact` turn finished; the injected reminder turn is running.
    AwaitingReminder,
    /// Compaction settled. The next dispatch skips the context check, since
    /// the latest turn metric still describes the pre-compaction context.
    JustCompacted,
}

/// What the dispatcher should do with the next pending item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionGate {
    /// No compaction in flight: check context usage before dispatching.
    Check,
    /// A compaction just settled: dispatch without checking.
    Proceed,
    /// A compaction is in flight: leave the item pending.
    Hold,
}

impl CompactionPhase {
    /// Phase after a compaction boundary arrives.
    fn on_boundary(self) -> Self {
        match self {
            Self::Requested => Self::Compacted,
            other => other,
        }
    }

    /// Phase after a turn-terminating output. `Requested` with no boundary
    /// means the compaction did not happen (error, nothing to compact); move
    /// on rather than asking again and looping.
    fn on_turn_end(self) -> Self {
        match self {
            Self::Requested => Self::JustCompacted,
            Self::Compacted => Self::AwaitingReminder,
            Self::AwaitingReminder => Self::JustCompacted,
            Self::JustCompacted => Self::JustCompacted,
        }
    }
}

impl SessionManager {
    /// Record that `/compact` was sent ahead of the next queued item.
    pub(crate) fn edit_stack_compaction_requested(&self, session_id: Uuid) {
        self.edit_stack_compaction
            .insert(session_id, (CompactionPhase::Requested, Instant::now()));
    }

    /// Advance on a Claude compaction boundary.
    pub(crate) fn edit_stack_compaction_boundary(&self, session_id: Uuid) {
        if let Some(mut entry) = self.edit_stack_compaction.get_mut(&session_id) {
            *entry = (entry.0.on_boundary(), Instant::now());
        }
    }

    /// Advance on a turn-terminating output, returning the new phase.
    pub(crate) fn edit_stack_compaction_turn_ended(
        &self,
        session_id: Uuid,
    ) -> Option<CompactionPhase> {
        let mut entry = self.edit_stack_compaction.get_mut(&session_id)?;
        *entry = (entry.0.on_turn_end(), Instant::now());
        Some(entry.0)
    }

    /// Release a stuck reminder wait. Returns whether it was still waiting.
    pub(crate) fn edit_stack_compaction_reminder_timed_out(&self, session_id: Uuid) -> bool {
        let Some(mut entry) = self.edit_stack_compaction.get_mut(&session_id) else {
            return false;
        };
        if entry.0 != CompactionPhase::AwaitingReminder {
            return false;
        }
        *entry = (CompactionPhase::JustCompacted, Instant::now());
        true
    }

    /// Decide what to do with the next pending item. Consumes `JustCompacted`.
    pub(crate) fn edit_stack_compaction_gate(&self, session_id: Uuid) -> CompactionGate {
        let Some((phase, since)) = self.edit_stack_compaction.get(&session_id).map(|e| *e) else {
            return CompactionGate::Check;
        };
        match phase {
            CompactionPhase::JustCompacted => {
                self.edit_stack_compaction.remove(&session_id);
                CompactionGate::Proceed
            }
            _ if since.elapsed() >= COMPACTION_HOLD_TTL => {
                self.edit_stack_compaction.remove(&session_id);
                CompactionGate::Proceed
            }
            _ => CompactionGate::Hold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_compaction_holds_until_reminder_result() {
        let sm = SessionManager::new();
        let id = Uuid::new_v4();
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Check);

        sm.edit_stack_compaction_requested(id);
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Hold);

        sm.edit_stack_compaction_boundary(id);
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Hold);

        // `/compact` result: the reminder turn is still to come.
        assert_eq!(
            sm.edit_stack_compaction_turn_ended(id),
            Some(CompactionPhase::AwaitingReminder)
        );
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Hold);

        // Reminder result: dispatch once without the stale context check.
        assert_eq!(
            sm.edit_stack_compaction_turn_ended(id),
            Some(CompactionPhase::JustCompacted)
        );
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Proceed);
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Check);
    }

    #[test]
    fn compaction_that_never_happened_does_not_loop() {
        let sm = SessionManager::new();
        let id = Uuid::new_v4();
        sm.edit_stack_compaction_requested(id);
        assert_eq!(
            sm.edit_stack_compaction_turn_ended(id),
            Some(CompactionPhase::JustCompacted)
        );
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Proceed);
    }

    #[test]
    fn unrelated_boundaries_and_turns_are_ignored() {
        let sm = SessionManager::new();
        let id = Uuid::new_v4();
        sm.edit_stack_compaction_boundary(id);
        assert_eq!(sm.edit_stack_compaction_turn_ended(id), None);
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Check);
    }

    #[test]
    fn reminder_timeout_releases_only_a_waiting_session() {
        let sm = SessionManager::new();
        let id = Uuid::new_v4();
        assert!(!sm.edit_stack_compaction_reminder_timed_out(id));
        sm.edit_stack_compaction_requested(id);
        assert!(!sm.edit_stack_compaction_reminder_timed_out(id));
        sm.edit_stack_compaction_boundary(id);
        sm.edit_stack_compaction_turn_ended(id);
        assert!(sm.edit_stack_compaction_reminder_timed_out(id));
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Proceed);
    }

    #[test]
    fn stale_hold_expires() {
        let sm = SessionManager::new();
        let id = Uuid::new_v4();
        let old = Instant::now() - COMPACTION_HOLD_TTL - Duration::from_secs(1);
        sm.edit_stack_compaction
            .insert(id, (CompactionPhase::Requested, old));
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Proceed);
        assert_eq!(sm.edit_stack_compaction_gate(id), CompactionGate::Check);
    }
}

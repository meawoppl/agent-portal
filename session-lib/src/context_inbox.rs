//! Plugin context changes the launcher posts to a running session (#2031).
//!
//! Two things travel together. `current` is the plugin context a *fresh*
//! conversation should start with, replacing the launch-time one, so `/clear`
//! never brings back plugins the user turned off. `pending` is the change
//! still to be told to the agent on its next input; it stays until that input
//! is accepted, so a failed delivery is retried rather than lost.

use std::sync::{Arc, Mutex};

/// A change waiting for the agent's next input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextUpdate {
    pub revision: u64,
    pub text: String,
}

#[derive(Debug, Default)]
struct State {
    /// The newest revision accepted; anything older is stale.
    revision: u64,
    current: Option<String>,
    pending: Option<ContextUpdate>,
}

/// Shared by the launcher, which posts, and the session, which delivers.
/// Live state only: never persisted.
#[derive(Debug, Clone, Default)]
pub struct ContextInbox(Arc<Mutex<State>>);

impl ContextInbox {
    /// Post revision `revision`: `current` becomes the plugin context for fresh
    /// conversations and `update` waits for the next input. A revision no
    /// newer than one already posted is stale and changes nothing; false then,
    /// or when the inbox is unusable.
    pub fn post(&self, revision: u64, current: String, update: String) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        if revision <= state.revision {
            return false;
        }
        state.revision = revision;
        state.current = Some(current);
        state.pending = Some(ContextUpdate {
            revision,
            text: update,
        });
        true
    }

    /// The plugin context for a fresh conversation, if it changed since launch.
    pub fn current(&self) -> Option<String> {
        self.0.lock().ok()?.current.clone()
    }

    /// The change not yet delivered, left in place until acknowledged.
    pub fn pending(&self) -> Option<ContextUpdate> {
        self.0.lock().ok()?.pending.clone()
    }

    /// `current` and `pending` read together, so they belong to one revision.
    pub fn snapshot(&self) -> (Option<String>, Option<ContextUpdate>) {
        match self.0.lock() {
            Ok(state) => (state.current.clone(), state.pending.clone()),
            Err(_) => (None, None),
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending().is_some()
    }

    /// The change at `revision` reached the agent, or a fresh conversation
    /// that already starts with `current` made it moot. A newer change posted
    /// meanwhile stays pending.
    pub fn acknowledge(&self, revision: u64) {
        if let Ok(mut state) = self.0.lock() {
            if state
                .pending
                .as_ref()
                .is_some_and(|p| p.revision == revision)
            {
                state.pending = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_revision_never_overwrites_a_newer_one() {
        let inbox = ContextInbox::default();
        assert!(inbox.post(2, "current two".into(), "update two".into()));
        // A slower job that resolved older state finishes afterwards.
        assert!(!inbox.post(1, "current one".into(), "update one".into()));
        assert_eq!(inbox.current().as_deref(), Some("current two"));
        assert_eq!(inbox.pending().unwrap().revision, 2);
    }

    #[test]
    fn pending_survives_until_its_own_revision_is_acknowledged() {
        let inbox = ContextInbox::default();
        inbox.post(1, "c1".into(), "u1".into());
        let first = inbox.pending().unwrap();
        // Delivery failed: nothing acknowledged, still pending.
        assert_eq!(inbox.pending(), Some(first.clone()));
        // A newer change arrives before the retry lands.
        inbox.post(2, "c2".into(), "u2".into());
        inbox.acknowledge(first.revision);
        assert_eq!(inbox.pending().unwrap().text, "u2");
        inbox.acknowledge(2);
        assert!(!inbox.is_pending());
        // `current` stays for fresh conversations after delivery.
        assert_eq!(inbox.current().as_deref(), Some("c2"));
    }
}

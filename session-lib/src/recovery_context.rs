//! Recovered reference text is consumed only after an input carrying it is
//! accepted. A pre-send rejection releases the claim for the next input.

use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct State {
    text: Option<String>,
    claimed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryContext(Arc<Mutex<State>>);

impl RecoveryContext {
    pub fn new(text: Option<String>) -> Self {
        Self(Arc::new(Mutex::new(State {
            text,
            claimed: false,
        })))
    }

    pub fn claim(&self) -> Option<String> {
        let mut state = self.0.lock().ok()?;
        if state.claimed {
            return None;
        }
        let text = state.text.clone()?;
        state.claimed = true;
        Some(text)
    }

    pub fn settle(&self, accepted: bool) {
        if let Ok(mut state) = self.0.lock() {
            if accepted {
                state.text = None;
            }
            state.claimed = false;
        }
    }

    /// A pending claim must survive snapshots until accepted, including while
    /// waiting for settings acknowledgement or a delivery receipt.
    pub fn snapshot(&self) -> Option<String> {
        self.0.lock().ok()?.text.clone()
    }
}

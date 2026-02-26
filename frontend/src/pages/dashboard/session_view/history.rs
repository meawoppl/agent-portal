//! Command history management for SessionView

use uuid::Uuid;
use web_sys::Storage;

/// Maximum number of commands to keep in history
pub const MAX_HISTORY: usize = 100;

/// Command history state with browser localStorage persistence
#[derive(Default)]
pub struct CommandHistory {
    /// History entries (most recent last)
    entries: Vec<String>,
    /// Current position in history (None = new input, Some(i) = viewing entries[i])
    position: Option<usize>,
    /// Draft input preserved when navigating history
    draft: String,
    /// Session ID for localStorage key
    session_id: Option<Uuid>,
}

impl CommandHistory {
    /// Create a new empty command history (no persistence, for tests)
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a command history for a specific session with localStorage persistence
    pub fn for_session(session_id: Uuid) -> Self {
        let mut history = Self {
            session_id: Some(session_id),
            ..Default::default()
        };
        history.load_from_storage();
        history
    }

    /// Get the localStorage key for this session
    fn storage_key(&self) -> Option<String> {
        self.session_id.map(|id| format!("command_history_{}", id))
    }

    /// Get localStorage handle
    fn get_storage() -> Option<Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    /// Load history from localStorage
    fn load_from_storage(&mut self) {
        let Some(key) = self.storage_key() else {
            return;
        };
        let Some(storage) = Self::get_storage() else {
            return;
        };

        if let Ok(Some(data)) = storage.get_item(&key) {
            if let Ok(entries) = serde_json::from_str::<Vec<String>>(&data) {
                self.entries = entries;
                // Trim to max if somehow over limit
                if self.entries.len() > MAX_HISTORY {
                    let excess = self.entries.len() - MAX_HISTORY;
                    self.entries.drain(0..excess);
                }
            }
        }
    }

    /// Save history to localStorage
    fn save_to_storage(&self) {
        let Some(key) = self.storage_key() else {
            return;
        };
        let Some(storage) = Self::get_storage() else {
            return;
        };

        if let Ok(data) = serde_json::to_string(&self.entries) {
            let _ = storage.set_item(&key, &data);
        }
    }

    /// Add a command to history (avoids consecutive duplicates)
    pub fn push(&mut self, command: String) {
        if self.entries.last() != Some(&command) {
            self.entries.push(command);
            if self.entries.len() > MAX_HISTORY {
                self.entries.remove(0);
            }
            self.save_to_storage();
        }
        // Reset navigation
        self.position = None;
        self.draft.clear();
    }

    /// Navigate up (older) in history
    /// Returns the command to display, or None if no change
    pub fn navigate_up(&mut self, current_input: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }

        match self.position {
            None => {
                // First time pressing up - save current input as draft
                self.draft = current_input.to_string();
                let pos = self.entries.len() - 1;
                self.position = Some(pos);
                Some(self.entries[pos].clone())
            }
            Some(pos) if pos > 0 => {
                // Go to older command
                let new_pos = pos - 1;
                self.position = Some(new_pos);
                Some(self.entries[new_pos].clone())
            }
            _ => {
                // Already at oldest
                None
            }
        }
    }

    /// Navigate down (newer) in history
    /// Returns the command to display, or None if no change
    pub fn navigate_down(&mut self) -> Option<String> {
        match self.position {
            Some(pos) if pos < self.entries.len() - 1 => {
                // Go to newer command
                let new_pos = pos + 1;
                self.position = Some(new_pos);
                Some(self.entries[new_pos].clone())
            }
            Some(_) => {
                // At newest history entry, go back to draft
                self.position = None;
                Some(self.draft.clone())
            }
            None => {
                // Not in history mode
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_navigate() {
        let mut history = CommandHistory::new();
        history.push("first".to_string());
        history.push("second".to_string());

        // Navigate up from empty input
        assert_eq!(history.navigate_up(""), Some("second".to_string()));
        assert_eq!(history.navigate_up(""), Some("first".to_string()));
        assert_eq!(history.navigate_up(""), None); // at oldest

        // Navigate down
        assert_eq!(history.navigate_down(), Some("second".to_string()));
        assert_eq!(history.navigate_down(), Some("".to_string())); // back to draft
    }

    #[test]
    fn test_preserves_draft() {
        let mut history = CommandHistory::new();
        history.push("old".to_string());

        // Start typing, then navigate up
        assert_eq!(history.navigate_up("my draft"), Some("old".to_string()));

        // Navigate back down to get draft
        assert_eq!(history.navigate_down(), Some("my draft".to_string()));
    }

    #[test]
    fn test_no_consecutive_duplicates() {
        let mut history = CommandHistory::new();
        history.push("same".to_string());
        history.push("same".to_string());
        history.push("same".to_string());

        assert_eq!(history.navigate_up(""), Some("same".to_string()));
        assert_eq!(history.navigate_up(""), None); // only one entry
    }
}

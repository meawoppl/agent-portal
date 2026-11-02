//! Durable reply receipts and the lightweight cross-session attention feed.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAttention {
    pub session_id: Uuid,
    pub reply_id: Option<Uuid>,
    pub reply_at: Option<String>,
    pub preview: String,
    pub unread: bool,
    pub needs_approval: bool,
    pub working: bool,
    pub connected: bool,
    pub muted: bool,
}
impl SessionAttention {
    pub fn ready(&self) -> bool {
        self.needs_approval || (self.unread && !self.working)
    }
    pub fn label(&self) -> &'static str {
        if self.needs_approval {
            "Needs approval"
        } else if self.working {
            "Working"
        } else if self.unread {
            "New reply"
        } else if !self.connected {
            "Offline"
        } else {
            "Idle"
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionAttentionResponse {
    pub sessions: Vec<SessionAttention>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateSessionAttention {
    pub client_id: Uuid,
    #[serde(default)]
    pub viewing: bool,
    /// Acknowledge only the reply actually rendered, never a later arrival.
    #[serde(default)]
    pub seen_reply_id: Option<Uuid>,
    #[serde(default)]
    pub muted: Option<bool>,
}

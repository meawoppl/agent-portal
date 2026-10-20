//! Native session discovery and import. Agent state stays on its launcher.
use crate::AgentType;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bound the control-plane reply. Large chats can import context without
/// copying scrollback; their native history is never truncated or rewritten.
pub const MAX_IMPORT_HISTORY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_IMPORT_MESSAGES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalSession {
    pub source_id: String,
    pub agent_type: AgentType,
    pub name: String,
    pub working_directory: String,
    pub updated_at: i64,
    pub model: Option<String>,
    pub portal_session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExternalSessionQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExternalSessionList {
    pub sessions: Vec<ExternalSession>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSessionRequest {
    pub source_id: String,
    pub include_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSessionResponse {
    pub session_id: Uuid,
    pub already_imported: bool,
}

/// Renderable snapshot only: these records must never enter the input queue,
/// run tools, or contribute to Portal's live usage accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedMessage {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedSessionImport {
    pub session: ExternalSession,
    pub messages: Vec<ImportedMessage>,
}

/// Stable across HTTP retries and concurrent imports, scoped to both owner
/// and launcher so identical native IDs on different computers never collide.
pub fn imported_session_id(user: Uuid, launcher: Uuid, agent: AgentType, source: &str) -> Uuid {
    Uuid::new_v5(
        &user,
        format!("portal-import-v1:{launcher}:{}:{source}", agent.as_str()).as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn import_identity_is_retry_safe_and_owner_host_agent_scoped() {
        let user = Uuid::new_v4();
        let host = Uuid::new_v4();
        let id = imported_session_id(user, host, AgentType::Codex, "thread");
        assert_eq!(
            id,
            imported_session_id(user, host, AgentType::Codex, "thread")
        );
        assert_ne!(
            id,
            imported_session_id(Uuid::new_v4(), host, AgentType::Codex, "thread")
        );
        assert_ne!(
            id,
            imported_session_id(user, Uuid::new_v4(), AgentType::Codex, "thread")
        );
        assert_ne!(
            id,
            imported_session_id(user, host, AgentType::Claude, "thread")
        );
        assert_ne!(
            id,
            imported_session_id(user, host, AgentType::Codex, "other")
        );
    }
}

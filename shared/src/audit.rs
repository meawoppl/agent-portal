//! Admin-only audit API. No credential values or arbitrary request payloads.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditCategory {
    #[default]
    Identity,
    Computers,
    Secrets,
}
impl AuditCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Computers => "computers",
            Self::Secrets => "secrets",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub occurred_at: String,
    pub category: String,
    pub event_type: String,
    pub actor_kind: String,
    pub actor_user_id: Option<Uuid>,
    pub actor_email: Option<String>,
    pub owner_user_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub launcher_id: Option<Uuid>,
    pub target_kind: String,
    pub target_id: Option<Uuid>,
    pub operation_id: Option<Uuid>,
    pub outcome: String,
    pub details: AuditDetails,
}
/// Only approved metadata fields can cross the writer/API boundary.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantee_user_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_read: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_use: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_write: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_manage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AuditQuery {
    pub category: Option<AuditCategory>,
    pub actor: Option<Uuid>,
    pub before_time: Option<String>,
    pub before_id: Option<Uuid>,
    pub limit: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AuditPage {
    pub events: Vec<AuditEvent>,
    pub has_more: bool,
    /// None explicitly means unlimited retention.
    pub retention_days: Option<u32>,
}

//! Secret metadata is separate from sensitive value and credential payloads.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SecretValue(pub String);
impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretScope {
    Personal,
    Shared,
    Organization,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretNamespace {
    Personal,
    Organization,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretRight {
    Read,
    Use,
    Write,
    Manage,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretGrant {
    pub user_id: Uuid,
    pub rights: Vec<SecretRight>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretMetadata {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub namespace: SecretNamespace,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub current_version: i32,
    pub generation: i64,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub deleted_by: Option<Uuid>,
    pub can_read: bool,
    pub can_use: bool,
    pub can_write: bool,
    pub can_manage: bool,
    pub grants: Vec<SecretGrant>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretListQuery {
    pub scope: Option<SecretScope>,
    #[serde(default)]
    pub deleted: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretListResponse {
    pub secrets: Vec<SecretMetadata>,
    pub can_manage_organization: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretCreateRequest {
    pub namespace: SecretNamespace,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub value: SecretValue,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretWriteRequest {
    pub expected_generation: i64,
    pub value: SecretValue,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMutationRequest {
    pub expected_generation: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRestoreRequest {
    pub expected_generation: i64,
    pub confirm_sharing: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretResolveRequest {
    pub reference: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretReadRequest {
    pub version: Option<i32>,
    #[serde(default)]
    pub for_execution: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretReadResponse {
    pub value: SecretValue,
    pub version: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretVersionMetadata {
    pub version: i32,
    pub created_at: String,
    pub created_by: Uuid,
    pub current: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretVersionsResponse {
    pub versions: Vec<SecretVersionMetadata>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretGrantRequest {
    pub expected_generation: i64,
    pub user_id: Uuid,
    pub rights: Vec<SecretRight>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretExpungeState {
    Queued,
    Discovering,
    Redacting,
    DeletingVersions,
    Verifying,
    CompleteActiveStorage,
    AwaitingBackupExpiry,
    Blocked,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretExpungeRequest {
    /// Cross-user cleanup is restricted to organization curators.
    #[serde(default)]
    pub global: bool,
    /// All retained versions are always included in the initial implementation.
    #[serde(default = "all_versions_default")]
    pub all_versions: bool,
}
fn all_versions_default() -> bool {
    true
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretExpungeJob {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub state: SecretExpungeState,
    pub records_redacted: i64,
    pub objects_rewritten: i64,
    pub bytes_scanned: i64,
    pub blocked_targets: i64,
    pub limitations: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMember {
    pub user_id: Uuid,
    pub display_name: String,
    pub email: String,
    #[serde(default)]
    pub identities: Vec<SecretIdentityMetadata>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMembersResponse {
    pub members: Vec<SecretMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretIdentityMetadata {
    pub provider: String,
    pub subject: String,
    pub handle: Option<String>,
    pub email: Option<String>,
    pub email_verified: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMemberResolveRequest {
    pub selector: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretBindingCreateRequest {
    pub secret_id: Uuid,
    pub session_id: Uuid,
    pub launcher_id: Uuid,
    pub version: Option<i32>,
    pub env_name: String,
    pub expected_generation: i64,
    pub expires_in_seconds: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretBindingMetadata {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub session_id: Uuid,
    pub launcher_id: Uuid,
    pub version: i32,
    pub env_name: String,
    pub generation: i64,
    pub expires_at: String,
    pub revoked_at: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretBindingListResponse {
    pub bindings: Vec<SecretBindingMetadata>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretBindingTicketResponse {
    pub ticket: SecretValue,
    pub expires_in: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretBindingRedeemRequest {
    pub ticket: SecretValue,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretBindingRedeemResponse {
    pub value: SecretValue,
    pub env_name: String,
    pub version: i32,
}

/// Server-owned state for a request to let one bound agent use a pinned value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretAccessState {
    Pending,
    Approved,
    Denied,
    Expired,
    Invalidated,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAccessCreateRequest {
    pub request_id: Uuid,
    pub reference: String,
    pub env_name: String,
    pub version: Option<i32>,
    pub expires_in_seconds: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAccessDecisionRequest {
    pub approve: bool,
}
/// Values and tickets never appear in the request card or its history marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretAccessRequestInfo {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub owner_user_id: Uuid,
    pub namespace: SecretNamespace,
    pub name: String,
    pub version: i32,
    pub session_id: Uuid,
    pub launcher_id: Uuid,
    pub env_name: String,
    pub expires_in_seconds: u64,
    pub state: SecretAccessState,
    pub created_at: String,
    pub expires_at: String,
    pub binding_id: Option<Uuid>,
}

/// A bounded, all-or-nothing approval set. Its item order is part of retry identity.
pub const MAX_SECRET_ACCESS_GROUP_ITEMS: usize = 16;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAccessGroupCreateRequest {
    pub request_id: Uuid,
    pub items: Vec<SecretAccessGroupItem>,
    pub expires_in_seconds: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAccessGroupItem {
    pub reference: String,
    pub env_name: String,
    pub version: Option<i32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretAccessGroupInfo {
    pub id: Uuid,
    pub state: SecretAccessState,
    pub items: Vec<SecretAccessRequestInfo>,
}

#[cfg(test)]
mod access_request_tests {
    use super::*;
    #[test]
    fn marker_is_only_a_pointer_and_unknown_states_fail_closed() {
        let id = Uuid::new_v4();
        let content = crate::PortalContent::SecretAccessRequest { request_id: id };
        let wire = serde_json::to_value(&content).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"type":"secret_access_request","request_id":id})
        );
        let parsed: crate::PortalContent = serde_json::from_value(wire).unwrap();
        assert!(
            matches!(parsed, crate::PortalContent::SecretAccessRequest { request_id } if request_id == id)
        );
        assert!(serde_json::from_str::<SecretAccessState>("\"anything_else\"").is_err());
    }
}

/// Browser-only explicit verification; no secret values or provider tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretVerificationStatus {
    pub verified: bool,
    pub providers: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretVerificationStart {
    pub provider: String,
    pub return_to: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretVerificationRedirect {
    pub url: String,
}

/// A write-only submission: values never appear in proposal/status responses.
#[derive(Debug, Serialize, Deserialize)]
pub struct SecretDepositCreate {
    pub request_id: Uuid,
    pub items: Vec<SecretDepositInput>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SecretDepositInput {
    pub namespace: SecretNamespace,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub value: SecretValue,
    /// Registered google:EMAIL, verified email, or Portal user UUID; read/use only.
    #[serde(default)]
    pub share: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretDepositRecipient {
    pub user_id: Uuid,
    pub display_name: String,
    pub email: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretDepositUpdate {
    pub version_id: Uuid,
    pub version: i32,
    pub generation: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretDepositItem {
    /// Absent for old create-only proposals; pins the exact update target.
    #[serde(default)]
    pub update: Option<SecretDepositUpdate>,
    pub secret_id: Uuid,
    pub version_id: Uuid,
    pub namespace: SecretNamespace,
    pub name: String,
    pub description: String,
    pub recipients: Vec<SecretDepositRecipient>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretDepositInfo {
    pub id: Uuid,
    pub session_id: Uuid,
    pub state: String,
    pub expires_at: String,
    pub items: Vec<SecretDepositItem>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SecretDepositDecision {
    pub approve: bool,
    /// Explicitly acknowledge every replacement; older create-only UIs fail closed.
    #[serde(default)]
    pub confirm_updates: Vec<Uuid>,
    /// Optional owner edits, in original item order. Recipient IDs may only be removed.
    pub items: Option<Vec<SecretDepositEdit>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretDepositEdit {
    pub name: String,
    pub description: String,
    pub recipients: Vec<Uuid>,
}

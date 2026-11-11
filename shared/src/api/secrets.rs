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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAuthorizeStartRequest {
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAuthorizeStartResponse {
    pub device_code: SecretValue,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAuthorizeApproveRequest {
    pub user_code: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAuthorizePollRequest {
    pub device_code: SecretValue,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretAuthorizeState {
    Pending,
    Approved,
    Denied,
    Expired,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAuthorizePollResponse {
    pub state: SecretAuthorizeState,
    pub credential: Option<SecretValue>,
    pub expires_at: Option<String>,
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
pub struct SecretCredentialMetadata {
    pub id: Uuid,
    pub label: String,
    pub created_at: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretCredentialsResponse {
    pub credentials: Vec<SecretCredentialMetadata>,
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

//! Settings API request/response types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One external login attached to the current portal account (#1535).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedIdentity {
    pub id: Uuid,
    pub provider: String,
    pub email: Option<String>,
    /// ISO-8601 timestamp; kept as a string so the settings wire type stays
    /// equally cheap on native and WASM targets.
    pub linked_at: String,
}

/// Response for `GET /api/settings/identities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedIdentitiesResponse {
    pub identities: Vec<LinkedIdentity>,
}

/// Response for GET /api/settings/sound
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundSettingsResponse {
    pub sound_config: Option<serde_json::Value>,
}

/// Body for `PUT /api/settings/profile` (#1485). A `nickname` of `None` or an
/// empty/whitespace string clears the nickname (falls back to name/email).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateProfileRequest {
    #[serde(default)]
    pub nickname: Option<String>,
}

/// Response for POST /api/stt/transcribe.
///
/// Empty `text` is a success, not a failure: it means the recording contained
/// no speech, and the caller should quietly do nothing rather than surface an
/// error the user can't act on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResponse {
    pub text: String,
}

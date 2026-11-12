//! Portal-owned references on normalized Codex generated-image and image-view items.
//! Provider `fileId` is never interpreted as a Portal image identifier.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexImageMedia {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<Uuid>,
    #[serde(default)]
    pub uploaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_path: Option<String>,
}

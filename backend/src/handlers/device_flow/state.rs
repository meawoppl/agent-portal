use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

// In-memory store for device flow state
// In production, use Redis or database
pub type DeviceFlowStore = Arc<RwLock<HashMap<String, DeviceFlowState>>>;

#[derive(Debug, Clone)]
pub struct DeviceFlowState {
    pub device_code: String,
    pub user_code: String,
    pub user_id: Option<Uuid>,
    pub access_token: Option<String>,
    pub expires_at: std::time::SystemTime,
    pub status: DeviceFlowStatus,
    /// Hostname of the machine requesting authorization
    pub hostname: Option<String>,
    /// Working directory / repository path
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceFlowStatus {
    Pending,
    Complete,
    Expired,
    Denied,
}

#[derive(Debug, Serialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status")]
pub enum PollResponse {
    #[serde(rename = "pending")]
    Pending,

    #[serde(rename = "complete")]
    Complete {
        access_token: String,
        user_id: String,
        user_email: String,
    },

    #[serde(rename = "expired")]
    Expired,

    #[serde(rename = "denied")]
    Denied,
}

#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    pub user_code: Option<String>,
}

pub(super) fn generate_user_code() -> String {
    let chars: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(|c| c as char)
        .collect::<String>()
        .to_uppercase();

    // Format as XXX-XXX for readability
    format!("{}-{}", &chars[0..3], &chars[3..6])
}

pub(super) fn generate_device_code() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(|c| c as char)
        .collect()
}

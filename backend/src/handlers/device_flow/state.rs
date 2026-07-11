use rand::{distributions::Alphanumeric, Rng};
use serde::Deserialize;
use shared::api::DeviceClientType;
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
    /// Client class requested when the device flow was initiated.
    pub client_type: DeviceClientType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceFlowStatus {
    Pending,
    Complete,
    Expired,
    Denied,
}

#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    pub user_code: Option<String>,
}

pub(super) fn generate_user_code() -> String {
    generate_user_code_with(&mut rand::thread_rng())
}

/// Generate a user code from a caller-supplied RNG. Split out from
/// [`generate_user_code`] so tests can drive it with a seeded RNG and stay
/// deterministic (see #1133).
pub(super) fn generate_user_code_with<R: Rng>(rng: &mut R) -> String {
    let chars: String = rng
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(|c| c as char)
        .collect::<String>()
        .to_uppercase();

    // Format as XXX-XXX for readability
    format!("{}-{}", &chars[0..3], &chars[3..6])
}

pub(super) fn generate_device_code() -> String {
    generate_device_code_with(&mut rand::thread_rng())
}

/// Generate a device code from a caller-supplied RNG (see [`generate_user_code_with`]).
pub(super) fn generate_device_code_with<R: Rng>(rng: &mut R) -> String {
    rng.sample_iter(&Alphanumeric)
        .take(32)
        .map(|c| c as char)
        .collect()
}

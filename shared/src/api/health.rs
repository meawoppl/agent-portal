//! Health-check endpoint response types.

use serde::{Deserialize, Serialize};

/// Response from GET /api/health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

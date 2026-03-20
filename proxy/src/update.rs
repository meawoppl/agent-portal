//! Auto-update functionality for the agent-portal binary.
//!
//! Thin wrapper around `portal_update` with the correct binary prefix.

pub use portal_update::{apply_pending_update, UpdateResult};

const BINARY_PREFIX: &str = "claude-portal";

/// Check for updates from GitHub releases
pub async fn check_for_update_github(check_only: bool) -> anyhow::Result<UpdateResult> {
    portal_update::check_for_update(BINARY_PREFIX, check_only).await
}

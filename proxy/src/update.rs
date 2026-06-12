//! Auto-update functionality for the agent-portal binary.
//!
//! Thin wrapper around `portal_update` with the correct binary prefix.

pub use portal_update::UpdateResult;

const BINARY_PREFIX: &str = "claude-portal";

/// Check for updates from GitHub releases
pub async fn check_for_update_github(check_only: bool) -> anyhow::Result<UpdateResult> {
    portal_update::check_for_update(BINARY_PREFIX, check_only).await
}

/// Startup auto-update ceremony: apply any pending update, then — when
/// `check` is true — check for and install the latest release. Returns
/// Ok(true) if the binary was replaced and the process should restart.
pub async fn startup_auto_update(check: bool) -> anyhow::Result<bool> {
    portal_update::startup_auto_update(BINARY_PREFIX, check).await
}

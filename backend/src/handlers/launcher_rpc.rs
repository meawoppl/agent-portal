//! Shared owner gate and HTTP error mapping for agent-management launcher RPCs.

use super::websocket::{LauncherRpcError, LauncherRpcKind};
use crate::{errors::AppError, AppState};
use shared::{LauncherToServer, ServerToLauncher};
use uuid::Uuid;

/// Host-side operations require the connected launcher's owner. Keep this gate
/// shared by request/reply operations and fire-and-forget restart/cancel paths.
pub(super) fn require_launcher_owner(
    state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
) -> Result<(), AppError> {
    let owner = state
        .session_manager
        .launcher_owner(launcher_id)
        .ok_or(AppError::NotFound("Launcher not found"))?;
    if owner != user_id {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

pub(super) async fn launcher_rpc(
    state: &AppState,
    launcher_id: Uuid,
    request_id: Uuid,
    message: ServerToLauncher,
    timeout_secs: u64,
) -> Result<LauncherToServer, AppError> {
    state
        .session_manager
        .request_launcher(
            launcher_id,
            LauncherRpcKind::Agent,
            request_id,
            message,
            std::time::Duration::from_secs(timeout_secs),
        )
        .await
        .map_err(|error| match error {
            LauncherRpcError::Disconnected => AppError::BadGateway("Launcher is not connected"),
            LauncherRpcError::Closed => {
                AppError::Internal("Launcher response channel closed".into())
            }
            LauncherRpcError::Timeout => {
                AppError::GatewayTimeout("Launcher did not respond in time")
            }
        })
}

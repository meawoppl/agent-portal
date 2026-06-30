//! Permission-response arm of the proxy connection loop (#1165 item 3).
//!
//! Extracted from the `run_main_loop` `select!` so the god-loop reads as thin
//! dispatch. Translates a frontend [`PermissionResponseData`] into the library's
//! neutral [`PermissionResponse`](session_lib::io::PermissionResponse) and hands
//! it to the agent; a stale/failed response is logged, not fatal.

use tracing::{debug, warn};

use session_lib::agent::Agent;
use session_lib::io::PermissionResponse as LibPermissionResponse;
use session_lib::session::Session;

use crate::Permission;

use super::PermissionResponseData;

/// Forward a frontend permission response to the agent.
pub(super) async fn handle_permission_response<A: Agent>(
    claude_session: &mut Session<A>,
    perm_response: PermissionResponseData,
) {
    debug!("sending permission response to claude: {:?}", perm_response);

    // Build the library's neutral PermissionResponse.
    let lib_response = if perm_response.allow {
        let input = perm_response
            .input
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let permissions: Vec<Permission> = perm_response
            .permissions
            .iter()
            .map(Permission::from_suggestion)
            .collect();

        if permissions.is_empty() {
            LibPermissionResponse::allow_with_input(input)
        } else {
            LibPermissionResponse::allow_with_input_and_remember(input, permissions)
        }
    } else {
        let reason = perm_response
            .reason
            .unwrap_or_else(|| "User denied".to_string());
        LibPermissionResponse::deny_with_reason(reason)
    };

    if let Err(e) = claude_session
        .respond_permission(&perm_response.request_id, lib_response)
        .await
    {
        warn!("Permission response failed (stale request?): {}", e);
    }
}

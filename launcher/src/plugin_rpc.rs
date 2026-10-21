//! Named plugin actions the backend asks this host to run (`plugins.manage.v1`).
//! Callers pick an action; they never supply a command line.
use anyhow::Result;
use shared::api::{PluginAction, PluginRequest, PluginResponse};

pub(crate) fn handle(request: PluginRequest) -> Result<PluginResponse> {
    match request.action {
        PluginAction::Inventory => Ok(PluginResponse {
            success: true,
            inventory: Some(crate::session_plugins::host_inventory(
                request.working_directory,
            )),
            ..Default::default()
        }),
        PluginAction::Install { .. }
        | PluginAction::Update { .. }
        | PluginAction::Remove { .. }
        | PluginAction::Doctor { .. }
        | PluginAction::SetPolicy { .. }
        | PluginAction::Surface { .. }
        | PluginAction::RunCommand { .. } => {
            anyhow::bail!("This launcher cannot run that plugin action yet")
        }
    }
}

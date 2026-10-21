//! Named plugin actions the backend asks this host to run (`plugins.manage.v1`).
//! Callers pick an action; they never supply a command line.
use anyhow::{Context, Result};
use shared::api::{PluginAction, PluginInventoryResponse, PluginRequest, PluginResponse};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Below the backend's 600 s action timeout, so a hung setup is killed here
/// and reported rather than left running after the request gave up.
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(570);
/// Output kept from one action; the tail carries the failure.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// One lifecycle change at a time: two installs racing on the same checkout
/// would defeat the CLI's move-aside-and-restore transaction.
static LIFECYCLE: Mutex<()> = Mutex::new(());

pub(crate) fn handle(request: PluginRequest) -> Result<PluginResponse> {
    let directory = request.working_directory;
    let lifecycle = |args: Vec<String>| -> Result<PluginResponse> {
        let _guard = LIFECYCLE
            .lock()
            .map_err(|_| anyhow::anyhow!("Plugin lifecycle lock poisoned"))?;
        let mut response = run_cli(&args)?;
        response.inventory = Some(crate::session_plugins::host_inventory(directory.clone()));
        Ok(response)
    };
    match request.action {
        PluginAction::Inventory => Ok(inventory_response(crate::session_plugins::host_inventory(
            directory,
        ))),
        PluginAction::CheckUpdates => Ok(inventory_response(with_updates(
            crate::session_plugins::host_inventory(directory),
        ))),
        PluginAction::Install { source, force } => {
            let mut args = vec!["install".to_string(), source];
            if force {
                args.push("--force".into());
            }
            lifecycle(args)
        }
        PluginAction::Update { name } => lifecycle(vec!["update".into(), name]),
        PluginAction::Remove { name } => lifecycle(vec!["remove".into(), name]),
        PluginAction::Doctor { name } => lifecycle(vec!["doctor".into(), name]),
        PluginAction::SetPolicy { .. }
        | PluginAction::Surface { .. }
        | PluginAction::RunCommand { .. } => {
            anyhow::bail!("This launcher cannot run that plugin action yet")
        }
    }
}

fn inventory_response(inventory: PluginInventoryResponse) -> PluginResponse {
    PluginResponse {
        success: true,
        inventory: Some(inventory),
        ..Default::default()
    }
}

/// Ask every plugin's source whether it has moved on, in parallel: each check
/// is a network round trip.
fn with_updates(mut inventory: PluginInventoryResponse) -> PluginInventoryResponse {
    let config = crate::config::load_config();
    std::thread::scope(|scope| {
        let checks: Vec<_> = inventory
            .plugins
            .iter()
            .map(|plugin| {
                let installed = config.plugins.get(&plugin.name);
                scope.spawn(move || installed.and_then(crate::plugin::update_available))
            })
            .collect();
        for (plugin, check) in inventory.plugins.iter_mut().zip(checks) {
            plugin.update_available = check.join().ok().flatten();
        }
    });
    inventory
}

/// Run `agent-portal plugin <args>` and capture its interleaved output. The
/// CLI owns the install/update transaction (backup, setup, restore on
/// failure); running it as a child keeps that logic in one place and lets
/// setup commands' output be captured with it.
fn run_cli(args: &[String]) -> Result<PluginResponse> {
    let exe = crate::service::current_exe_path()?;
    let log_dir = session_lib::paths::config_dir().join("plugin-actions");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("{}.log", uuid::Uuid::new_v4()));
    let log = std::fs::File::create(&log_path)?;
    let mut child = Command::new(&exe)
        .arg("plugin")
        .args(args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .with_context(|| format!("Failed to run {exe} plugin {}", args.join(" ")))?;
    let deadline = Instant::now() + LIFECYCLE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let output = std::fs::read(&log_path).unwrap_or_default();
    let _ = std::fs::remove_file(&log_path);
    let output = tail(&output, MAX_OUTPUT_BYTES);
    let command = format!("agent-portal plugin {}", args.join(" "));
    Ok(match status {
        None => PluginResponse {
            success: false,
            error: Some(format!(
                "`{command}` did not finish within {} minutes and was stopped",
                LIFECYCLE_TIMEOUT.as_secs() / 60
            )),
            output,
            ..Default::default()
        },
        Some(status) => PluginResponse {
            success: status.success(),
            error: (!status.success()).then(|| format!("`{command}` failed ({status})")),
            output,
            exit_code: status.code(),
            ..Default::default()
        },
    })
}

/// The last `max` bytes of `bytes` as text, cut at a line boundary.
fn tail(bytes: &[u8], max: usize) -> String {
    if bytes.len() <= max {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let cut = &bytes[bytes.len() - max..];
    let start = cut.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
    format!("…\n{}", String::from_utf8_lossy(&cut[start..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_whole_trailing_lines() {
        assert_eq!(tail(b"short", 64), "short");
        assert_eq!(tail(b"first line\nsecond\nthird", 12), "…\nthird");
    }
}

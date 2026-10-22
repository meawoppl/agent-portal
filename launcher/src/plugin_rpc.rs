//! Named plugin actions the backend asks this host to run (`plugins.manage.v1`).
//! Callers pick an action; they never supply a command line.
use anyhow::{Context, Result};
use shared::api::{
    PluginAction, PluginInventoryResponse, PluginRequest, PluginResponse, PluginSurfaceAction,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Below the backend's 600 s action timeout, so a hung setup is killed here
/// and reported rather than left running after the request gave up.
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(570);
/// Output kept from one action; the tail carries the failure.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// An action whose log passes this is stopped: a setup script stuck printing
/// must not fill the disk.
const MAX_LOG_BYTES: u64 = 32 * 1024 * 1024;

/// One plugin change at a time: two installs racing on a checkout would defeat
/// the CLI's move-aside-and-restore transaction, and a surface starting while
/// its plugin is removed would race the same files. A second request is
/// turned away rather than queued, since waiting would eat into its own time
/// budget. An atomic flag rather than a mutex, so the claim can be held across
/// a surface start's awaits.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Held while a plugin change runs; releasing it lets the next one in.
struct Claim;

impl Claim {
    fn take() -> Result<Self> {
        if BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            anyhow::bail!(
                "Another plugin action is running on this computer; try again when it finishes"
            );
        }
        Ok(Self)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// Run one action. Surface actions wait on health checks and run here; the
/// rest block on files and child processes and run on a blocking thread.
pub(crate) async fn handle(request: PluginRequest) -> Result<PluginResponse> {
    if let PluginAction::Surface { name, action } = &request.action {
        let session = request
            .session_id
            .zip(request.working_directory.as_deref())
            .map(|(id, dir)| (id.to_string(), std::path::PathBuf::from(dir)));
        let _claim = (*action != PluginSurfaceAction::Status)
            .then(Claim::take)
            .transpose()?;
        let surface = crate::plugin::surface_action(name, *action, session).await?;
        return Ok(PluginResponse {
            success: true,
            surface: Some(surface),
            ..Default::default()
        });
    }
    tokio::task::spawn_blocking(move || handle_blocking(request)).await?
}

fn handle_blocking(request: PluginRequest) -> Result<PluginResponse> {
    let directory = request.working_directory;
    let lifecycle = |args: Vec<String>| -> Result<PluginResponse> {
        let _claim = Claim::take()?;
        let mut response = run_cli(&args)?;
        response.inventory = Some(crate::session_plugins::host_inventory(directory.clone())?);
        Ok(response)
    };
    match request.action {
        PluginAction::Inventory => Ok(inventory_response(crate::session_plugins::host_inventory(
            directory,
        )?)),
        PluginAction::CheckUpdates => Ok(inventory_response(with_updates(
            crate::session_plugins::host_inventory(directory)?,
        ))),
        PluginAction::SetPolicy { name, policy } => {
            let Some(project) = directory.as_deref() else {
                anyhow::bail!("Choose a project directory to set a plugin policy for");
            };
            anyhow::ensure!(
                crate::config::load_config().plugins.contains_key(&name),
                "Plugin is not installed: {name}"
            );
            crate::plugin_policy::set(project, &name, policy)?;
            Ok(inventory_response(crate::session_plugins::host_inventory(
                directory,
            )?))
        }
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
        PluginAction::Surface { .. } => unreachable!("surface actions run in `handle`"),
        PluginAction::RunCommand { .. } => {
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
    let mut command = Command::new(&exe);
    command.arg("plugin").args(args);
    let log_dir = session_lib::paths::config_dir().join("plugin-actions");
    run_captured(
        command,
        &format!("agent-portal plugin {}", args.join(" ")),
        &log_dir,
        LIFECYCLE_TIMEOUT,
    )
}

/// Run `command` with its output in a log under `log_dir`, and report how it
/// ended. The child leads its own process group, so stopping it on a timeout
/// or runaway output also stops whatever its setup scripts started, before
/// the lifecycle lock is released.
fn run_captured(
    mut command: Command,
    label: &str,
    log_dir: &Path,
    timeout: Duration,
) -> Result<PluginResponse> {
    std::fs::create_dir_all(log_dir)?;
    let log_path = log_dir.join(format!("{}.log", uuid::Uuid::new_v4()));
    let log = std::fs::File::create(&log_path)?;
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to run {label}"))?;
    let deadline = Instant::now() + timeout;
    let stopped = loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "`{label}` did not finish within {} s and was stopped",
                timeout.as_secs()
            ));
        }
        if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
            break Err(format!(
                "`{label}` wrote more than {} MiB of output and was stopped",
                MAX_LOG_BYTES >> 20
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if stopped.is_err() {
        kill_tree(&mut child);
    }
    let output = read_tail(&log_path, MAX_OUTPUT_BYTES);
    let _ = std::fs::remove_file(&log_path);
    Ok(match stopped {
        Err(error) => PluginResponse {
            success: false,
            error: Some(error),
            output,
            ..Default::default()
        },
        Ok(status) => PluginResponse {
            success: status.success(),
            error: (!status.success()).then(|| format!("`{label}` failed ({status})")),
            output,
            exit_code: status.code(),
            ..Default::default()
        },
    })
}

/// Stop the child and everything it started, then reap it.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    session_lib::session::signal_process_group(child.id(), libc::SIGKILL);
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// The last `max` bytes of the file at `path` as text, cut at a line
/// boundary, without reading the rest.
fn read_tail(path: &Path, max: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max as u64);
    let mut bytes = Vec::with_capacity(max.min(len as usize));
    if file.seek(SeekFrom::Start(start)).is_err()
        || file.take(max as u64).read_to_end(&mut bytes).is_err()
    {
        return String::new();
    }
    if start == 0 {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let line = bytes.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
    format!("…\n{}", String::from_utf8_lossy(&bytes[line..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tail_keeps_whole_trailing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, "short").unwrap();
        assert_eq!(read_tail(&path, 64), "short");
        std::fs::write(&path, "first line\nsecond\nthird").unwrap();
        assert_eq!(read_tail(&path, 12), "…\nthird");
        assert_eq!(read_tail(&dir.path().join("missing"), 12), "");
    }

    /// A hung setup is stopped along with what it started: the backgrounded
    /// child must not survive to keep changing the checkout.
    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_stops_the_whole_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "echo started; sleep 30 & echo $! > {}; wait",
            pid_file.display()
        ));
        let response = run_captured(
            command,
            "hung setup",
            &dir.path().join("logs"),
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(!response.success);
        assert!(response.error.unwrap().contains("did not finish"));
        assert!(response.output.contains("started"));
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Killed but not yet reaped by its new parent is a zombie, not alive.
        let running = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit(") ")
                    .next()
                    .is_some_and(|s| !s.starts_with('Z'))
            })
        };
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            !running()
        });
        assert!(gone, "the backgrounded grandchild outlived the timeout");
        assert!(
            std::fs::read_dir(dir.path().join("logs"))
                .unwrap()
                .next()
                .is_none(),
            "the action log is removed"
        );
    }

    /// Lifecycle and surface changes share one claim: while any runs, the
    /// others are turned away instead of racing the same plugin files. A
    /// status check needs no claim.
    #[tokio::test]
    async fn plugin_changes_exclude_each_other() {
        let held = Claim::take().unwrap();
        let busy = |action| PluginRequest {
            working_directory: None,
            session_id: None,
            action,
        };
        for action in [
            PluginAction::Remove {
                name: "anything".into(),
            },
            PluginAction::Surface {
                name: "anything".into(),
                action: PluginSurfaceAction::Stop,
            },
            PluginAction::Surface {
                name: "anything".into(),
                action: PluginSurfaceAction::Start,
            },
        ] {
            let error = handle(busy(action)).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Another plugin action is running"),
                "{error:#}"
            );
        }
        // Status isn't turned away by the claim (it fails later, on the
        // unknown plugin, for an unrelated reason).
        let status = handle(busy(PluginAction::Surface {
            name: "anything".into(),
            action: PluginSurfaceAction::Status,
        }))
        .await
        .unwrap_err();
        assert!(!status.to_string().contains("Another plugin action"));
        drop(held);
        assert!(Claim::take().is_ok(), "released with its holder");
    }
}

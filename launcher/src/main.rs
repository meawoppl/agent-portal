// Ratchet for the workspace unwrap/expect deny (#1165 item 8): this crate
// still has production unwrap/expect; remove this allow as it is cleaned.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod config;
mod connection;
mod forward;
mod message;
mod pastebin;
mod path_policy;
mod process_manager;
mod scheduler;
mod service;

use clap::{Parser, Subcommand};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "agent-portal")]
#[command(about = "Persistent daemon that launches claude-portal sessions as in-process tasks")]
#[command(
    after_help = "Source & issues: https://github.com/meawoppl/agent-portal\n  \
                  Report bugs / file issues: https://github.com/meawoppl/agent-portal/issues"
)]
struct Args {
    /// Backend WebSocket URL (default: wss://txcl.io in release, ws://localhost:3000 in debug)
    #[arg(long)]
    backend_url: Option<String>,

    /// JWT auth token for the launcher
    #[arg(long, env = "LAUNCHER_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Human-readable name for this launcher (default: hostname)
    #[arg(long)]
    name: Option<String>,

    /// Maximum concurrent sessions. Each session spawns a Claude CLI child
    /// process with its own memory and CPU footprint, so unbounded concurrency
    /// can exhaust system resources and degrade performance for every session.
    /// The default of 20 is a conservative starting point for a typical
    /// developer machine; tune upward on larger hosts if needed.
    #[arg(long, default_value_t = 20)]
    max_sessions: usize,

    /// Development mode (no auth required)
    #[arg(long)]
    dev: bool,

    /// Skip the automatic update check on startup
    #[arg(long)]
    no_update: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Authenticate with the backend server via browser
    Login,
    /// Update agent-portal to the latest version (restarts service if running)
    Update,
    /// Manage the launcher system service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Message your other agent sessions
    Message {
        #[command(subcommand)]
        action: MessageAction,
    },
    /// Expose a local HTTP port through the portal for the user's browser.
    ///
    /// A session forwards one port at a time (front multiple services behind
    /// your own reverse proxy). `forward <port>` sets it and prints the URL,
    /// replacing any current forward; `forward list` shows it; `forward close`
    /// revokes it.
    Forward {
        /// A port number, `list`, or `close`.
        target: String,
    },
}

#[derive(Subcommand, Debug)]
enum MessageAction {
    /// List your sessions (agents) you can message
    List,
    /// Send a message into another session's agent
    Send {
        /// Target session id
        agent_id: String,
        /// Message text
        message: String,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// Install and start the launcher as a persistent service
    Install,
    /// Stop and remove the launcher service
    Uninstall,
    /// Show the current service status
    Status,
    /// Start the launcher service
    Start,
    /// Stop the launcher service
    Stop,
    /// Restart the launcher service
    Restart,
    /// Show service logs
    Logs {
        /// Number of lines to show
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: u32,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
    /// Upload system info, build info, and logs to an unlisted paste
    Pastebin,
}

pub(crate) const BINARY_PREFIX: &str = "agent-portal";

fn resolve_backend_url(args_url: Option<String>, config_url: Option<String>) -> String {
    args_url
        .or(config_url)
        .unwrap_or_else(|| shared::default_backend_url().to_string())
}

/// True when some PATH entry contains an *executable* regular file named
/// `name` — the same contract a child's `execvp` will apply. A stale
/// non-executable file (e.g. a half-finished copy in `~/.local/bin`) must not
/// count as resolvable, or we'd skip the fix and spawned agents would hit
/// exec/permission errors anyway.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_has_executable(path: &str, name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.split(':').filter(|dir| !dir.is_empty()).any(|dir| {
        std::path::Path::new(dir)
            .join(name)
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// Make sure `agent-portal` resolves on this process's PATH, prepending the
/// running binary's own directory when it doesn't. Spawned agents inherit the
/// fixed PATH. Leaves PATH alone when some `agent-portal` already resolves —
/// deliberate installs (e.g. a symlink in `~/.local/bin`) keep winning.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ensure_self_on_path() {
    let path = std::env::var("PATH").unwrap_or_default();
    if path_has_executable(&path, BINARY_PREFIX) {
        return;
    }
    let Ok(exe) = service::current_exe_path() else {
        return;
    };
    let Some(dir) = std::path::Path::new(&exe)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
    else {
        return;
    };
    let fixed = service::path_with_dir(&path, &dir);
    if fixed != path {
        // Racy in theory on a multithreaded runtime, but nothing reads PATH
        // concurrently during startup and children only inherit it at spawn.
        std::env::set_var("PATH", &fixed);
        info!("agent-portal not on PATH; prepended own directory {}", dir);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn ensure_self_on_path() {}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod path_tests {
    use super::path_has_executable;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn non_executable_file_is_not_resolvable() {
        let dir = std::env::temp_dir().join(format!("ap-exec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("agent-portal");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            !path_has_executable(&dir.to_string_lossy(), "agent-portal"),
            "non-executable file must not count as resolvable"
        );

        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(path_has_executable(&dir.to_string_lossy(), "agent-portal"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_and_empty_entries_are_skipped() {
        assert!(!path_has_executable("", "agent-portal"));
        assert!(!path_has_executable(
            "/nonexistent-dir::/also-missing",
            "agent-portal"
        ));
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let args = Args::parse();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Handle subcommands before the daemon startup path
    match args.command {
        Some(Command::Login) => return cmd_login(&args).await,
        Some(Command::Update) => return cmd_update().await,
        Some(Command::Service { action }) => {
            return match action {
                ServiceAction::Install => service::install(),
                ServiceAction::Uninstall => service::uninstall(),
                ServiceAction::Status => service::status(),
                ServiceAction::Start => service::start(),
                ServiceAction::Stop => service::stop(),
                ServiceAction::Restart => service::restart(),
                ServiceAction::Logs { lines, follow } => service::logs(lines, follow),
                ServiceAction::Pastebin => pastebin::upload_diagnostics().await,
            };
        }
        Some(Command::Message { action }) => {
            return match action {
                MessageAction::List => message::list().await,
                MessageAction::Send { agent_id, message } => {
                    message::send(&agent_id, &message).await
                }
            };
        }
        Some(Command::Forward { target }) => {
            return match target.as_str() {
                "list" => forward::list().await,
                "close" => forward::close().await,
                other => match other.parse::<u16>() {
                    Ok(p) => forward::open(p).await,
                    Err(_) => Err(anyhow::anyhow!(
                        "expected a port number, `list`, or `close`, got `{other}`"
                    )),
                },
            };
        }
        None => {}
    }

    // --- Daemon startup path ---

    // Agents this daemon spawns shell out to `agent-portal` (messaging, port
    // forwarding) and inherit our environment. Under systemd/launchd the
    // service PATH often doesn't cover wherever this binary actually lives, so
    // if `agent-portal` doesn't resolve, prepend our own directory. The unit
    // generator bakes the same directory into `Environment=PATH` (self-heals
    // via `service::sync` after updates); this covers the running process and
    // units that predate that.
    ensure_self_on_path();

    // Check if running as a system service; suggest installing if not
    if !args.no_update && !service::is_installed() {
        eprintln!();
        eprintln!("  Tip: Install agent-portal as a system service for persistent operation:");
        eprintln!("    agent-portal service install");
        eprintln!();
    }

    // Apply pending updates, then auto-update on startup (unless --no-update)
    match portal_update::startup_auto_update(BINARY_PREFIX, !args.no_update).await {
        Ok(true) => {
            info!("Launcher updated, please restart");
            std::process::exit(0);
        }
        Ok(false) => {}
        Err(e) => {
            warn!(
                "Update check failed: {}. Continuing with current version.",
                e
            );
        }
    }

    let config = config::load_config();

    // CLI args override config file, which overrides the compile-time default
    let backend_url = resolve_backend_url(args.backend_url, config.backend_url);

    // A CLI/env-supplied token pins auth for this run; otherwise the
    // connection loop re-reads launcher.json on every attempt so a parked
    // launcher picks up `agent-portal login` without a restart (#1237).
    // Bootstrap the device flow when neither source has a token.
    if args.auth_token.is_none() && config.auth_token.is_none() && !args.dev {
        tracing::info!("No auth token found, starting device flow authentication");
        let result = portal_auth::device_flow_login(&backend_url, None).await?;
        // Persist the resolved backend_url alongside the token: the token is
        // only valid against the server that minted it, so a plain restart or
        // the installed service (no --backend-url on its command line) must
        // reconnect to the same host.
        if let Err(e) = config::save_credentials(&backend_url, &result.access_token) {
            tracing::warn!("Failed to save credentials to config: {}", e);
        }
    }
    let launcher_name = args
        .name
        .or(config.name)
        .unwrap_or_else(claude_session_lib::hostname_or_unknown);

    let launcher_id = config::persistent_launcher_id();

    tracing::info!(
        "Starting launcher '{}' (id: {})",
        launcher_name,
        launcher_id
    );
    tracing::info!("Backend URL: {}", backend_url);
    tracing::debug!("Max sessions: {}", args.max_sessions);

    if !config.sessions.is_empty() {
        tracing::info!(
            "Discarding {} launcher-local expected session(s); backend DB is authoritative",
            config.sessions.len()
        );
        if let Err(e) = config::clear_sessions() {
            tracing::warn!("Failed to clear launcher-local expected sessions: {}", e);
        }
    }

    let (process_manager, exit_rx) =
        process_manager::ProcessManager::new(backend_url.clone(), args.max_sessions);

    connection::run_launcher_loop(
        &backend_url,
        launcher_id,
        &launcher_name,
        args.auth_token,
        process_manager,
        exit_rx,
    )
    .await
}

/// `agent-portal login` — authenticate via device flow and save the token
async fn cmd_login(args: &Args) -> anyhow::Result<()> {
    let config = config::load_config();
    let backend_url = resolve_backend_url(args.backend_url.clone(), config.backend_url);

    println!("Authenticating with {}...", backend_url);
    let result = portal_auth::device_flow_login(&backend_url, None).await?;
    // Store the resolved backend_url with the token so switching servers
    // (e.g. `login --backend-url wss://newhost`) sticks — a token minted by
    // one server won't authenticate against another.
    config::save_credentials(&backend_url, &result.access_token)?;
    println!();
    println!("Logged in as {}", result.user_email);
    println!(
        "Credentials for {} saved to {}",
        backend_url,
        config::config_path_display()
    );
    Ok(())
}

/// `agent-portal update` — update binary and restart service if running
async fn cmd_update() -> anyhow::Result<()> {
    match portal_update::startup_auto_update(BINARY_PREFIX, true).await {
        Ok(true) => {
            println!("agent-portal updated successfully.");
            // Restart the service if it's installed and running
            if service::is_installed() {
                service::sync()?;
                println!("Restarting system service...");
                service::restart()?;
                println!("Service restarted.");
            }
        }
        Ok(false) => {
            println!("agent-portal is already up to date.");
        }
        Err(e) => {
            anyhow::bail!("Update failed: {}", e);
        }
    }
    Ok(())
}

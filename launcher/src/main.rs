mod config;
mod connection;
mod process_manager;
mod service;

use clap::{Parser, Subcommand};
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "agent-portal")]
#[command(about = "Persistent daemon that launches claude-portal sessions as in-process tasks")]
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

    /// Maximum concurrent sessions
    #[arg(long)]
    max_sessions: Option<usize>,

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
    /// Install agent-portal as a persistent system service
    Install,
    /// Update agent-portal to the latest version (restarts service if running)
    Update,
    /// Manage the launcher system service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
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
}

const BINARY_PREFIX: &str = "agent-portal";

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
        Some(Command::Install) => return service::install(),
        Some(Command::Update) => return cmd_update().await,
        Some(Command::Service { action }) => {
            return match action {
                ServiceAction::Install => service::install(),
                ServiceAction::Uninstall => service::uninstall(),
                ServiceAction::Status => service::status(),
            };
        }
        None => {}
    }

    // --- Daemon startup path ---

    // Check if running as a system service; suggest installing if not
    if !args.no_update && !service::is_installed() {
        eprintln!();
        eprintln!("  Tip: Install agent-portal as a system service for persistent operation:");
        eprintln!("    agent-portal install");
        eprintln!();
    }

    // Apply pending updates (Windows only)
    if let Ok(true) = portal_update::apply_pending_update() {
        info!("Pending update applied successfully");
    }

    // Auto-update on startup (unless --no-update)
    if !args.no_update {
        match portal_update::check_for_update(BINARY_PREFIX, false).await {
            Ok(portal_update::UpdateResult::UpToDate) => {}
            Ok(portal_update::UpdateResult::Updated) => {
                info!("Launcher updated, please restart");
                std::process::exit(0);
            }
            Ok(portal_update::UpdateResult::UpdateAvailable { .. }) => {}
            Err(e) => {
                warn!(
                    "Update check failed: {}. Continuing with current version.",
                    e
                );
            }
        }
    }

    let config = config::load_config();

    // CLI args override config file, which overrides the compile-time default
    let backend_url = args
        .backend_url
        .or(config.backend_url)
        .unwrap_or_else(|| shared::default_backend_url().to_string());

    let auth_token = match args.auth_token.or(config.auth_token) {
        Some(token) => Some(token),
        None if args.dev => None,
        None => {
            tracing::info!("No auth token found, starting device flow authentication");
            let result = portal_auth::device_flow_login(&backend_url, None).await?;
            if let Err(e) = config::save_auth_token(&result.access_token) {
                tracing::warn!("Failed to save auth token to config: {}", e);
            }
            Some(result.access_token)
        }
    };
    let max_sessions = args.max_sessions.or(config.max_processes).unwrap_or(5);

    let launcher_name = args.name.or(config.name).unwrap_or_else(|| {
        hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    });

    let launcher_id = Uuid::new_v4();

    tracing::info!(
        "Starting launcher '{}' (id: {})",
        launcher_name,
        launcher_id
    );
    tracing::info!("Backend URL: {}", backend_url);
    tracing::info!("Max sessions: {}", max_sessions);

    if !config.sessions.is_empty() {
        tracing::info!("Expected sessions configured: {}", config.sessions.len());
        for s in &config.sessions {
            tracing::info!("  - {} ({})", s.working_directory, s.agent_type);
        }
    }

    let (process_manager, exit_rx) =
        process_manager::ProcessManager::new(backend_url.clone(), max_sessions);

    connection::run_launcher_loop(
        &backend_url,
        launcher_id,
        &launcher_name,
        auth_token.as_deref(),
        process_manager,
        exit_rx,
        config.sessions,
    )
    .await
}

/// `agent-portal login` — authenticate via device flow and save the token
async fn cmd_login(args: &Args) -> anyhow::Result<()> {
    let config = config::load_config();
    let backend_url = args
        .backend_url
        .clone()
        .or(config.backend_url)
        .unwrap_or_else(|| shared::default_backend_url().to_string());

    println!("Authenticating with {}...", backend_url);
    let result = portal_auth::device_flow_login(&backend_url, None).await?;
    config::save_auth_token(&result.access_token)?;
    println!();
    println!("Logged in as {}", result.user_email);
    Ok(())
}

/// `agent-portal update` — update binary and restart service if running
async fn cmd_update() -> anyhow::Result<()> {
    // Apply any pending updates first (Windows)
    if let Ok(true) = portal_update::apply_pending_update() {
        info!("Pending update applied successfully");
    }

    match portal_update::check_for_update(BINARY_PREFIX, false).await {
        Ok(portal_update::UpdateResult::UpToDate) => {
            println!("agent-portal is already up to date.");
        }
        Ok(portal_update::UpdateResult::Updated) => {
            println!("agent-portal updated successfully.");
            // Restart the service if it's installed and running
            if service::is_installed() {
                println!("Restarting system service...");
                service::restart()?;
                println!("Service restarted.");
            }
        }
        Ok(portal_update::UpdateResult::UpdateAvailable { version, .. }) => {
            println!("Update available: {}", version);
        }
        Err(e) => {
            anyhow::bail!("Update failed: {}", e);
        }
    }
    Ok(())
}

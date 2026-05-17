mod auth;
mod commands;
mod config;
mod session;
mod shim;
mod ui;
mod update;
mod util;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use claude_session_lib::{Session as ClaudeSession, SessionConfig};
use config::{ProxyConfig, SessionAuth};
use session::ProxySessionConfig;
use tracing::{info, warn};
use tracing_subscriber::prelude::*;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "claude-portal")]
#[command(about = "Wrapper for Claude CLI that proxies sessions to web interface")]
#[command(
    long_about = "A portal wrapper for Claude Code CLI that forwards your terminal sessions \
to a web interface for remote viewing and collaboration.\n\n\
QUICK START:\n  \
1. Get a setup token from the web interface\n  \
2. Run: claude-portal --init <token-url>\n  \
3. Start coding: claude-portal [claude args]\n\n\
CONFIG:\n  \
Configuration is stored in ~/.config/agent-portal/config.json and includes\n  \
the backend URL and authentication tokens per working directory."
)]
#[command(after_help = "EXAMPLES:\n  \
  # First-time setup with token from web UI\n  \
  claude-portal --init https://myserver.com/p/abc123\n\n  \
  # Start a new session in current directory\n  \
  claude-portal\n\n  \
  # Start with a custom session name\n  \
  claude-portal --session-name \"feature-xyz\"\n\n  \
  # Force a fresh session (don't resume previous)\n  \
  claude-portal --new-session\n\n  \
  # Pass arguments through to claude CLI\n  \
  claude-portal --model sonnet -- \"explain this code\"\n\n  \
  # Re-authenticate if token expired\n  \
  claude-portal --reauth")]
struct Args {
    /// Initialize proxy with a setup token from the web interface.
    ///
    /// The token URL is displayed in the web UI when you click "Add Session".
    /// This saves the backend URL and auth token to your local config.
    #[arg(long, value_name = "TOKEN_URL")]
    init: Option<String>,

    /// Override the backend server URL.
    ///
    /// Normally set via --init, but can be overridden for testing or
    /// connecting to a different server temporarily.
    #[arg(long, value_name = "URL")]
    backend_url: Option<String>,

    /// Provide authentication token directly (advanced).
    ///
    /// Skips the OAuth device flow. Useful for CI/CD or scripted usage.
    /// The token is a JWT issued by the backend server.
    #[arg(long, value_name = "JWT", env = "PORTAL_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Custom name for this session.
    ///
    /// If not provided, generates a name from hostname and timestamp.
    /// Session names appear in the web interface for identification.
    #[arg(long, value_name = "NAME")]
    session_name: Option<String>,

    /// Start a fresh session instead of resuming the previous one.
    ///
    /// By default, claude-portal resumes your last session in this directory.
    /// Use this flag to start with a clean slate.
    #[arg(long)]
    new_session: bool,

    /// Force re-authentication with the backend server.
    ///
    /// Use this if your cached auth token has expired or you need
    /// to switch accounts. Triggers the OAuth device flow again.
    #[arg(long)]
    reauth: bool,

    /// Remove cached authentication for this directory.
    ///
    /// Clears the saved auth token for the current working directory.
    /// You'll need to re-authenticate on next run.
    #[arg(long)]
    logout: bool,

    /// Development mode - bypass authentication entirely.
    ///
    /// Only works if the backend server is also running in dev mode.
    /// Useful for local development and testing.
    #[arg(long)]
    dev: bool,

    /// Skip the automatic update check on startup.
    ///
    /// By default, claude-portal checks for updates from the backend
    /// and auto-updates if a newer version is available.
    #[arg(long)]
    no_update: bool,

    /// Check for updates without installing.
    ///
    /// Checks if a newer version is available from GitHub releases
    /// and prints information about it without auto-updating.
    #[arg(long)]
    check_update: bool,

    /// Force update from GitHub releases.
    ///
    /// Downloads and installs the latest version from GitHub releases,
    /// bypassing the backend server.
    #[arg(long)]
    update: bool,

    /// Session ID for log tagging (set by launcher daemon).
    ///
    /// When provided, all log output is tagged with this session ID
    /// and output is switched to JSON format for machine parsing.
    #[arg(long, value_name = "UUID", hide = true)]
    session_id_tag: Option<Uuid>,

    /// Agent CLI to use: "claude" (default) or "codex".
    #[arg(long, value_name = "AGENT", default_value = "claude")]
    agent: String,

    /// Enable debug-level logging for troubleshooting.
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Run in shim mode (transparent proxy for VS Code extension).
    ///
    /// In shim mode, the proxy acts as a transparent stdin/stdout bridge
    /// between a parent process (e.g., VS Code Claude Code extension) and
    /// the claude CLI binary. All claude output is forwarded to stdout while
    /// also being sent to the portal backend via WebSocket. Input from both
    /// stdin and the portal web UI reaches claude.
    ///
    /// Diagnostic output goes to stderr only. No TUI banners are emitted.
    #[arg(long)]
    shim: bool,

    /// Arguments to pass through to the claude CLI.
    ///
    /// Everything after -- or unrecognized flags are forwarded to claude.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    claude_args: Vec<String>,
}

fn init_tracing(session_id_tag: Option<Uuid>, verbose: bool) {
    let default_level = if verbose { "debug" } else { "info" };
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_level.into());

    if let Some(sid) = session_id_tag {
        // Launched by daemon: JSON format with session_id field
        let fmt_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_target(false)
            .with_span_list(false);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .init();
        tracing::info!(session_id = %sid, "Proxy starting with session tag");
    } else {
        // Interactive: human-readable format
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }
}

fn default_session_name() -> String {
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    format!("{}-{}", hostname, timestamp)
}

/// Get the current git branch name, if in a git repository
fn get_git_branch(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    // If we're in detached HEAD state, get the short commit hash instead
    if branch == "HEAD" {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| format!("detached:{}", s.trim()))
    } else {
        Some(branch)
    }
}

/// Handle --check-update: check for updates without installing
async fn handle_check_update() -> Result<()> {
    ui::print_checking_for_updates();

    match update::check_for_update_github(true).await {
        Ok(update::UpdateResult::UpToDate) => {
            ui::print_up_to_date();
            Ok(())
        }
        Ok(update::UpdateResult::UpdateAvailable {
            version,
            download_url,
        }) => {
            ui::print_update_available(&version, &download_url);
            Ok(())
        }
        Ok(update::UpdateResult::Updated) => {
            // Shouldn't happen with check_only=true
            ui::print_update_complete();
            Ok(())
        }
        Err(e) => {
            ui::print_update_check_failed(&e.to_string());
            Err(e)
        }
    }
}

/// Handle --update: force update from GitHub releases
async fn handle_force_update() -> Result<()> {
    ui::print_updating_from_github();

    match update::check_for_update_github(false).await {
        Ok(update::UpdateResult::UpToDate) => {
            ui::print_up_to_date();
            Ok(())
        }
        Ok(update::UpdateResult::Updated) => {
            ui::print_update_complete();
            Ok(())
        }
        Ok(update::UpdateResult::UpdateAvailable { .. }) => {
            // Shouldn't happen with check_only=false
            Ok(())
        }
        Err(e) => {
            ui::print_update_failed(&e.to_string());
            Err(e)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let args = Args::parse();

    init_tracing(args.session_id_tag, args.verbose);

    // Skip update checks and UI output entirely in shim mode
    if !args.shim {
        if let Ok(true) = update::apply_pending_update() {
            ui::print_pending_update_applied();
        }

        // Handle explicit update commands first
        if args.check_update {
            return handle_check_update().await;
        }

        if args.update {
            return handle_force_update().await;
        }

        // Check for updates before anything else (unless --no-update or --init/--logout)
        if !args.no_update && args.init.is_none() && !args.logout {
            match update::check_for_update_github(false).await {
                Ok(update::UpdateResult::UpToDate) => {
                    // Continue normally
                }
                Ok(update::UpdateResult::Updated) => {
                    ui::print_update_complete();
                    std::process::exit(0);
                }
                Ok(update::UpdateResult::UpdateAvailable { .. }) => {
                    // Shouldn't happen since check_only=false, but handle gracefully
                }
                Err(e) => {
                    warn!(
                        "Update check failed: {}. Continuing with current version.",
                        e
                    );
                }
            }
        }
    }

    let cwd = std::env::current_dir()
        .context("Failed to get current directory")?
        .to_string_lossy()
        .to_string();

    let mut config = ProxyConfig::load().context("Failed to load config file")?;

    // Handle subcommands that exit early
    if args.logout {
        return commands::handle_logout(&mut config, &cwd);
    }

    if let Some(ref init_value) = args.init {
        return commands::handle_init(&mut config, &cwd, init_value, args.backend_url.as_deref());
    }

    // Resolve session (new or resume)
    let (session_id, session_name, resuming) = resolve_session(&args, &cwd)?;

    // Resolve backend URL: CLI arg > per-directory config > global default > compile-time default
    let backend_url = args
        .backend_url
        .clone()
        .or_else(|| config.get_backend_url(&cwd).map(|s| s.to_string()))
        .or_else(|| config.preferences.default_backend_url.clone())
        .unwrap_or_else(|| shared::default_backend_url().to_string());

    // Print startup info (suppress in shim mode — stdout is reserved for claude I/O)
    if !args.shim {
        ui::print_startup_banner();
        if args.session_id_tag.is_none() {
            ui::print_deprecation_warning();
        }
        ui::print_session_info(
            &session_name,
            &session_id.to_string(),
            &backend_url,
            resuming,
        );
    }

    // Resolve auth token — in shim mode, auth failure is non-fatal so claude
    // still launches even if the portal backend is unreachable.
    let auth_token = if args.shim {
        match resolve_auth_token(&args, &mut config, &cwd, &backend_url).await {
            Ok(token) => token,
            Err(e) => {
                warn!("Portal auth failed (shim will continue without it): {}", e);
                None
            }
        }
    } else {
        resolve_auth_token(&args, &mut config, &cwd, &backend_url).await?
    };

    // Detect git branch
    let git_branch = get_git_branch(&cwd);
    if let Some(ref branch) = git_branch {
        info!("Detected git branch: {}", branch);
    }

    // Parse agent type
    let agent_type: shared::AgentType =
        args.agent.parse().map_err(|e: String| anyhow::anyhow!(e))?;

    // Build session config
    let session_config = ProxySessionConfig {
        backend_url,
        session_id,
        session_name,
        auth_token,
        working_directory: cwd,
        resume: resuming,
        git_branch,
        claude_args: args.claude_args.clone(),
        replaces_session_id: None,
        launcher_id: None,
        agent_type,
        scheduled_task_id: None,
    };

    // Branch: shim mode or normal proxy mode
    // run_shim calls std::process::exit with claude's exit code
    if args.shim {
        shim::run_shim(session_config).await?;
        return Ok(());
    }

    // Start Claude and run session
    run_proxy_session(session_config).await
}

/// Resolve which session to use (new or resume existing)
fn resolve_session(args: &Args, cwd: &str) -> Result<(Uuid, String, bool)> {
    let (mut config, lock) =
        ProxyConfig::load_locked().context("Failed to load config with lock")?;

    let existing_session = config.get_directory_session(cwd).cloned();

    // Force new session or no existing session to resume
    if args.new_session || existing_session.is_none() {
        let had_existing = existing_session.is_some();

        // Start a new session
        let session_id = Uuid::new_v4();
        let session_name = args
            .session_name
            .clone()
            .unwrap_or_else(default_session_name);

        let dir_session = ProxyConfig::create_directory_session(session_id, session_name.clone());
        config.set_directory_session(cwd.to_string(), dir_session);
        config.save_with_lock(&lock)?;

        if args.new_session && had_existing {
            warn!(
                "Starting new session (--new-session flag) - previous session will not be resumed"
            );
            if !args.shim {
                ui::print_new_session_forced();
            }
        } else if !had_existing {
            info!(
                "No existing session for directory {}, creating new session {}",
                cwd, session_id
            );
            if !args.shim {
                ui::print_no_previous_session();
            }
        }

        info!("New session ID: {}", session_id);
        Ok((session_id, session_name, false))
    } else if let Some(existing) = existing_session {
        // Resume existing session
        let session_name = args
            .session_name
            .clone()
            .unwrap_or_else(|| existing.session_name.clone());

        config.touch_directory_session(cwd);
        config.save_with_lock(&lock)?;

        info!(
            "Resuming session {} (created: {}, last used: {})",
            existing.session_id, existing.created_at, existing.last_used
        );
        if !args.shim {
            ui::print_resuming_session(&existing.session_id.to_string(), &existing.created_at);
        }

        Ok((existing.session_id, session_name, true))
    } else {
        // This branch is unreachable: if first condition is false, existing_session must be Some
        unreachable!("existing_session cannot be None here")
    }
}

/// Resolve the authentication token
async fn resolve_auth_token(
    args: &Args,
    config: &mut ProxyConfig,
    cwd: &str,
    backend_url: &str,
) -> Result<Option<String>> {
    if args.dev {
        if !args.shim {
            ui::print_dev_mode();
        }
        return Ok(None);
    }

    if let Some(ref token) = args.auth_token {
        return Ok(Some(token.clone()));
    }

    if !args.reauth {
        if let Some(session_auth) = config.get_session_auth(cwd) {
            if !args.shim {
                ui::print_user(session_auth.user_email.as_deref().unwrap_or("unknown user"));
            }
            return Ok(Some(session_auth.auth_token.clone()));
        }
    }

    // In shim mode, never trigger interactive device flow — it blocks Claude
    // startup and println! would corrupt the JSON protocol on stdout.
    if args.shim {
        // Try any cached token (cross-directory fallback) before giving up
        if let Some(any_auth) = config.get_any_session_auth() {
            info!("Using cached token from another directory for shim mode");
            return Ok(Some(any_auth.auth_token.clone()));
        }
        warn!("No cached auth token for shim mode. Run 'claude-portal' from a terminal to authenticate.");
        return Ok(None);
    }

    // Need to authenticate
    info!("Authenticating via device flow");
    let result = auth::device_flow_login(backend_url, Some(cwd)).await?;

    config.set_session_auth(
        cwd.to_string(),
        SessionAuth {
            user_id: result.user_id,
            auth_token: result.access_token.clone(),
            user_email: Some(result.user_email),
            last_used: chrono::Utc::now().to_rfc3339(),
            backend_url: None,
            session_prefix: None,
        },
    );
    config.atomic_save()?;

    Ok(Some(result.access_token))
}

/// Start Claude and run the proxy session
async fn run_proxy_session(mut config: ProxySessionConfig) -> Result<()> {
    loop {
        ui::print_status("Starting Claude CLI...");

        let mut claude_session = create_claude_session(&config).await?;

        ui::print_started();

        // Create input channel (shared across reconnections)
        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Run the connection loop
        let result =
            session::run_connection_loop(&config, &mut claude_session, input_tx, &mut input_rx)
                .await;

        let _ = claude_session.stop().await;

        match result {
            Ok(session::LoopResult::NormalExit) => {
                info!("Proxy shutting down");
                return Ok(());
            }
            Ok(session::LoopResult::SessionNotFound) => {
                warn!("Session not found (from JSON output), will start fresh session");
                // Only retry if we were trying to resume
                if !config.resume {
                    return Ok(());
                }
            }
            Err(e) => {
                return Err(e);
            }
        }

        // Create a new session and update config
        let old_session_id = config.session_id;
        let new_session_id = Uuid::new_v4();
        info!(
            "Previous session {} not found locally, starting fresh session {}",
            old_session_id, new_session_id
        );
        ui::print_session_not_found(&old_session_id.to_string());

        // Update the directory_sessions config with the new session ID
        let (mut proxy_config, lock) = ProxyConfig::load_locked()
            .context("Failed to load config with lock for session update")?;

        let dir_session =
            ProxyConfig::create_directory_session(new_session_id, config.session_name.clone());
        proxy_config.set_directory_session(config.working_directory.clone(), dir_session);
        proxy_config.save_with_lock(&lock)?;

        // Update the session config for retry
        config.session_id = new_session_id;
        config.resume = false;
        config.replaces_session_id = Some(old_session_id);

        info!("Retrying with new session ID: {}", new_session_id);
        // Loop will continue and start fresh session
    }
}

/// Create a Claude session using claude-session-lib
async fn create_claude_session(config: &ProxySessionConfig) -> Result<ClaudeSession> {
    let claude_config = SessionConfig {
        session_id: config.session_id,
        working_directory: PathBuf::from(&config.working_directory),
        session_name: config.session_name.clone(),
        resume: config.resume,
        claude_path: None,
        extra_args: config.claude_args.clone(),
        agent_type: config.agent_type,
    };

    if config.resume {
        info!(
            "Using --resume {} to resume Claude session",
            config.session_id
        );
    } else {
        info!(
            "Starting fresh Claude session with ID {}",
            config.session_id
        );
    }

    ClaudeSession::new(claude_config)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create Claude session: {}", e))
}

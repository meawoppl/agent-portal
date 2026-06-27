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
use claude_session_lib::{default_session_name, ClaudeAgent};
use session_lib::{Session, SessionConfig};
use shared::api::{ResolveProxySessionRequest, ResolveProxySessionResponse};

/// Type alias — the proxy binary only ever drives Claude sessions; the
/// launcher is where heterogeneous (Claude + Codex) dispatch lives.
type ClaudeSession = Session<ClaudeAgent>;
use config::{DirectorySession, ProxyConfig, SessionAuth};
use session::{get_git_branch, ProxySessionConfig};
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
  claude-portal --reauth\n\n\
  Source & issues: https://github.com/meawoppl/agent-portal\n  \
  Report bugs / file issues: https://github.com/meawoppl/agent-portal/issues")]
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

/// Handle --check-update / --update: query GitHub releases and either
/// report on a newer version (`check_only`) or install it.
async fn handle_update(check_only: bool) -> Result<()> {
    if check_only {
        ui::print_checking_for_updates();
    } else {
        ui::print_updating_from_github();
    }

    match update::check_for_update_github(check_only).await {
        Ok(update::UpdateResult::UpToDate) => {
            ui::print_up_to_date();
            Ok(())
        }
        Ok(update::UpdateResult::UpdateAvailable {
            version,
            download_url,
        }) => {
            // Only returned with check_only=true
            ui::print_update_available(&version, &download_url);
            Ok(())
        }
        Ok(update::UpdateResult::Updated) => {
            // Only returned with check_only=false
            ui::print_update_complete();
            Ok(())
        }
        Err(e) => {
            if check_only {
                ui::print_update_check_failed(&e.to_string());
            } else {
                ui::print_update_failed(&e.to_string());
            }
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
        // Apply any pending update, then auto-update before anything else —
        // unless updates are disabled (--no-update, --init, --logout) or an
        // explicit update command is handled below with its own UI.
        let auto_check = !args.no_update
            && !args.check_update
            && !args.update
            && args.init.is_none()
            && !args.logout;
        match update::startup_auto_update(auto_check).await {
            Ok(true) => {
                ui::print_update_complete();
                std::process::exit(0);
            }
            Ok(false) => {
                // Continue normally
            }
            Err(e) => {
                warn!(
                    "Update check failed: {}. Continuing with current version.",
                    e
                );
            }
        }

        // Handle explicit update commands
        if args.check_update {
            return handle_update(true).await;
        }

        if args.update {
            return handle_update(false).await;
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

    // Resolve backend URL: CLI arg > per-directory config > global default > compile-time default
    let backend_url = args
        .backend_url
        .clone()
        .or_else(|| config.get_backend_url(&cwd).map(|s| s.to_string()))
        .or_else(|| config.preferences.default_backend_url.clone())
        .unwrap_or_else(|| shared::default_backend_url().to_string());

    // Parse agent type
    let agent_type: shared::AgentType =
        args.agent.parse().map_err(|e: String| anyhow::anyhow!(e))?;

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

    // Resolve session (new or resume). Prefer the backend's DB-backed session
    // identity; fall back to the local directory cache only when the backend
    // cannot answer (offline, auth unavailable, older server).
    let resolved_session =
        resolve_session(&args, &cwd, &backend_url, auth_token.as_deref(), agent_type).await?;
    let session_id = resolved_session.session_id;
    let session_name = resolved_session.session_name;
    let resuming = resolved_session.resuming;

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

    // Detect git branch
    let git_branch = get_git_branch(&cwd);
    if let Some(ref branch) = git_branch {
        info!("Detected git branch: {}", branch);
    }

    // Persist-back sink for the codex thread id learned from
    // `thread.started` / `thread/resume`. The closure captures the
    // working directory so it can stamp the right DirectorySession row;
    // load+update+save is atomic via `ProxyConfig::load_locked`. No-op
    // path for claude sessions (the codex io-task is the only emitter).
    let codex_thread_id_sink: claude_session_lib::proxy_session::CodexThreadIdSink = {
        let cwd_for_sink = cwd.clone();
        std::sync::Arc::new(move |thread_id: String| {
            let (mut cfg, lock) = match ProxyConfig::load_locked() {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("codex_thread_id_sink: load_locked failed: {}", e);
                    return;
                }
            };
            let updated = match cfg.get_directory_session(&cwd_for_sink) {
                Some(existing) => DirectorySession {
                    codex_thread_id: Some(thread_id),
                    ..existing.clone()
                },
                None => {
                    // Directory record vanished between launch and the
                    // codex thread.started event — unusual but recoverable.
                    // Skip rather than fabricate a session_id row that
                    // doesn't match the in-memory session.
                    warn!(
                        "codex_thread_id_sink: no DirectorySession for {:?}, skipping persist",
                        cwd_for_sink
                    );
                    return;
                }
            };
            cfg.set_directory_session(cwd_for_sink.clone(), updated);
            if let Err(e) = cfg.save_with_lock(&lock) {
                warn!("codex_thread_id_sink: save_with_lock failed: {}", e);
            }
        })
    };

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
        codex_thread_id_sink: Some(codex_thread_id_sink),
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

struct ResolvedSession {
    session_id: Uuid,
    session_name: String,
    resuming: bool,
}

/// Resolve which session to use (new or resume existing).
async fn resolve_session(
    args: &Args,
    cwd: &str,
    backend_url: &str,
    auth_token: Option<&str>,
    agent_type: shared::AgentType,
) -> Result<ResolvedSession> {
    if args.new_session {
        return create_fresh_local_session(args, cwd, true);
    }

    if let Some(token) = auth_token {
        match resolve_session_from_backend(args, cwd, backend_url, token, agent_type).await {
            Ok(Some(resolved)) => return Ok(resolved),
            Ok(None) => {
                info!("Backend has no resumable session for {}; creating new", cwd);
                return create_fresh_local_session(args, cwd, false);
            }
            Err(e) => {
                warn!(
                    "Backend session resolution failed (falling back to local cache): {}",
                    e
                );
            }
        }
    } else {
        info!("No auth token available; using local session cache");
    }

    resolve_session_from_local_cache(args, cwd)
}

async fn resolve_session_from_backend(
    args: &Args,
    cwd: &str,
    backend_url: &str,
    auth_token: &str,
    agent_type: shared::AgentType,
) -> Result<Option<ResolvedSession>> {
    let http_url = http_api_url(backend_url, "/api/proxy/resolve-session")?;
    let hostname = hostname::get().ok().and_then(|h| h.into_string().ok());
    let req = ResolveProxySessionRequest {
        auth_token: Some(auth_token.to_string()),
        working_directory: cwd.to_string(),
        hostname,
        agent_type,
    };

    let response = reqwest::Client::new()
        .post(http_url)
        .json(&req)
        .send()
        .await
        .context("request failed")?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        anyhow::bail!("server returned {}", response.status());
    }

    let resolved = response
        .json::<ResolveProxySessionResponse>()
        .await
        .context("invalid resolve-session response")?;

    let Some(session_id) = resolved.session_id else {
        return Ok(None);
    };
    let session_name = args
        .session_name
        .clone()
        .or(resolved.session_name.clone())
        .unwrap_or_else(default_session_name);

    cache_directory_session(
        cwd,
        session_id,
        session_name.clone(),
        resolved.created_at.clone(),
        resolved.last_activity.clone(),
    )?;

    if !args.shim {
        let created_at = resolved
            .created_at
            .as_deref()
            .unwrap_or("unknown creation time");
        ui::print_resuming_session(&session_id.to_string(), created_at);
    }

    Ok(Some(ResolvedSession {
        session_id,
        session_name,
        resuming: true,
    }))
}

fn resolve_session_from_local_cache(args: &Args, cwd: &str) -> Result<ResolvedSession> {
    let (mut config, lock) =
        ProxyConfig::load_locked().context("Failed to load config with lock")?;

    let existing_session = config.get_directory_session(cwd).cloned();

    if let Some(existing) = existing_session {
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

        Ok(ResolvedSession {
            session_id: existing.session_id,
            session_name,
            resuming: true,
        })
    } else {
        create_fresh_local_session(args, cwd, false)
    }
}

fn create_fresh_local_session(args: &Args, cwd: &str, forced: bool) -> Result<ResolvedSession> {
    let (mut config, lock) =
        ProxyConfig::load_locked().context("Failed to load config with lock")?;
    let had_existing = config.get_directory_session(cwd).is_some();
    let session_id = Uuid::new_v4();
    let session_name = args
        .session_name
        .clone()
        .unwrap_or_else(default_session_name);

    let dir_session = ProxyConfig::create_directory_session(session_id, session_name.clone());
    config.set_directory_session(cwd.to_string(), dir_session);
    config.save_with_lock(&lock)?;

    if forced && had_existing {
        warn!("Starting new session (--new-session flag) - previous session will not be resumed");
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
    Ok(ResolvedSession {
        session_id,
        session_name,
        resuming: false,
    })
}

fn cache_directory_session(
    cwd: &str,
    session_id: Uuid,
    session_name: String,
    created_at: Option<String>,
    last_used: Option<String>,
) -> Result<()> {
    let (mut config, lock) =
        ProxyConfig::load_locked().context("Failed to load config with lock")?;
    let mut dir_session = ProxyConfig::create_directory_session(session_id, session_name);
    if let Some(created_at) = created_at {
        dir_session.created_at = created_at;
    }
    if let Some(last_used) = last_used {
        dir_session.last_used = last_used;
    }
    config.set_directory_session(cwd.to_string(), dir_session);
    config.save_with_lock(&lock)
}

fn http_api_url(backend_url: &str, path: &str) -> Result<String> {
    let mut url = url::Url::parse(backend_url).context("Invalid backend URL")?;
    match url.scheme() {
        "ws" => url.set_scheme("http").ok(),
        "wss" => url.set_scheme("https").ok(),
        "http" | "https" => Some(()),
        other => anyhow::bail!("Unsupported backend URL scheme: {}", other),
    }
    .context("Failed to convert backend URL scheme")?;
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
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
            auth_token: result.access_token.clone(),
            user_email: Some(result.user_email),
            last_used: chrono::Utc::now().to_rfc3339(),
            backend_url: None,
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
        let (input_tx, mut input_rx) =
            tokio::sync::mpsc::unbounded_channel::<session::PortalInput>();

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
            Ok(session::LoopResult::RegistrationRejected) => {
                warn!("Registration rejected by server; re-authentication may be required");
                return Ok(());
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
    // For codex resumes we need to hand the io-task the app-server
    // thread id from the prior incarnation. Load it from the per-directory
    // record the proxy persisted on the previous launch. Missing on a
    // fresh launch, missing for claude sessions — both fine, the codex
    // io-task gates resume on `Some` + `config.resume`.
    let codex_thread_id = ProxyConfig::load().ok().and_then(|cfg| {
        cfg.get_directory_session(&config.working_directory)
            .and_then(|s| s.codex_thread_id.clone())
    });

    let claude_config = SessionConfig {
        session_id: config.session_id,
        working_directory: PathBuf::from(&config.working_directory),
        session_name: config.session_name.clone(),
        resume: config.resume,
        claude_path: None,
        extra_args: config.claude_args.clone(),
        agent_type: config.agent_type,
        codex_thread_id,
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

#[cfg(test)]
mod tests {
    use super::http_api_url;

    #[test]
    fn http_api_url_converts_ws_backend_url() {
        assert_eq!(
            http_api_url("ws://localhost:3000", "/api/proxy/resolve-session").unwrap(),
            "http://localhost:3000/api/proxy/resolve-session"
        );
        assert_eq!(
            http_api_url("wss://portal.example", "/api/proxy/resolve-session").unwrap(),
            "https://portal.example/api/proxy/resolve-session"
        );
    }

    #[test]
    fn http_api_url_replaces_existing_path_query_and_fragment() {
        assert_eq!(
            http_api_url(
                "wss://portal.example/base/path?x=1#frag",
                "/api/proxy/resolve-session"
            )
            .unwrap(),
            "https://portal.example/api/proxy/resolve-session"
        );
    }
}

//! Terminal UI helpers for the proxy CLI.

use colored::Colorize;
use std::io::Write;

/// Print the startup banner
pub fn print_startup_banner() {
    println!();
    println!(
        "{}",
        "╭──────────────────────────────────────╮".bright_blue()
    );
    println!(
        "{}",
        "│        Agent Portal Starting         │".bright_blue()
    );
    println!(
        "{}",
        "╰──────────────────────────────────────╯".bright_blue()
    );
    println!();
}

/// Print deprecation warning when running the proxy directly
pub fn print_deprecation_warning() {
    eprintln!(
        "  {} {}",
        "WARNING:".bright_yellow().bold(),
        "Running the proxy binary directly is deprecated.".yellow()
    );
    eprintln!(
        "  {}",
        "Use the `agent-launcher` service instead for persistent session management.".yellow()
    );
    eprintln!();
}

/// Print session information
pub fn print_session_info(session_name: &str, session_id: &str, backend_url: &str, resuming: bool) {
    println!("  {} {}", "Session:".dimmed(), session_name.bright_white());
    println!("  {} {}", "ID:".dimmed(), session_id[..8].bright_cyan());
    println!("  {} {}", "Backend:".dimmed(), backend_url.bright_white());
    println!(
        "  {} {}",
        "Mode:".dimmed(),
        if resuming {
            "resume".bright_yellow()
        } else {
            "new".bright_green()
        }
    );
    println!();
}

/// Print dev mode status
pub fn print_dev_mode() {
    println!(
        "  {} {}",
        "Mode:".dimmed(),
        "development (no auth)".bright_yellow()
    );
    println!();
}

/// Print authenticated user
pub fn print_user(email: &str) {
    println!("  {} {}", "User:".dimmed(), email.bright_white());
    println!();
}

/// Print "new session" status message
pub fn print_new_session_forced() {
    println!(
        "  {} {} Starting new session (--new-session flag)",
        "⚠".bright_yellow(),
        "WARNING:".bright_yellow()
    );
}

/// Print "no previous session" message
pub fn print_no_previous_session() {
    println!(
        "  {} No previous session found, starting fresh",
        "→".bright_blue()
    );
}

/// Print "resuming session" message
pub fn print_resuming_session(session_id: &str, created_at: &str) {
    println!(
        "  {} Resuming session {} from {}",
        "→".bright_green(),
        session_id[..8].bright_cyan(),
        created_at.bright_white()
    );
}

/// Print a status line with spinner prefix
pub fn print_status(message: &str) {
    print!("  {} {} ", "→".bright_blue(), message);
    let _ = std::io::stdout().flush();
}

/// Print "started" result
pub fn print_started() {
    println!("{}", "started".bright_green());
}

/// Print logout success
pub fn print_logout_success(email: &str) {
    println!("{} Logged out from {}", "✓".bright_green(), email);
}

/// Print no cached auth message
pub fn print_no_cached_auth() {
    println!("No cached authentication found for this directory");
}

/// Print init success
pub fn print_init_start(email: &str) {
    println!(
        "{} Initializing proxy with token for {}",
        "→".bright_blue(),
        email
    );
}

/// Print init complete
pub fn print_init_complete(email: &str, backend_url: &str) {
    println!("{} Configuration saved for {}", "✓".bright_green(), email);
    println!("  Backend: {}", backend_url);
    println!();
    println!(
        "You can now run {} without arguments.",
        "claude-portal".bright_cyan()
    );
}

/// Print session not found message (when resuming a session that doesn't exist locally)
pub fn print_session_not_found(session_id: &str) {
    println!();
    println!(
        "  {} Previous session {} not found locally",
        "⚠".bright_yellow(),
        session_id[..8].bright_cyan()
    );
    println!(
        "  {} Starting a fresh session instead...",
        "→".bright_blue()
    );
    println!();
}

/// Print update complete message
pub fn print_update_complete() {
    println!();
    println!(
        "{}",
        "╭──────────────────────────────────────╮".bright_green()
    );
    println!(
        "{}",
        "│         ✓ Update Installed           │".bright_green()
    );
    println!(
        "{}",
        "╰──────────────────────────────────────╯".bright_green()
    );
    println!();
    println!(
        "  A new version of {} has been installed.",
        "claude-portal".bright_cyan()
    );
    println!("  Please run the command again to use the updated version.");
    println!();
}

/// Print checking for updates message
pub fn print_checking_for_updates() {
    println!();
    println!(
        "  {} Checking for updates from GitHub...",
        "→".bright_blue()
    );
}

/// Print up to date message
pub fn print_up_to_date() {
    println!(
        "  {} {} is up to date.",
        "✓".bright_green(),
        "claude-portal".bright_cyan()
    );
    println!();
}

/// Print update available message
pub fn print_update_available(version: &str, download_url: &str) {
    println!();
    println!(
        "{}",
        "╭──────────────────────────────────────╮".bright_yellow()
    );
    println!(
        "{}",
        "│         Update Available             │".bright_yellow()
    );
    println!(
        "{}",
        "╰──────────────────────────────────────╯".bright_yellow()
    );
    println!();
    println!("  {} {}", "Version:".dimmed(), version.bright_white());
    println!();
    println!("  To update, run:");
    println!(
        "    {} {}",
        "$".dimmed(),
        "claude-portal --update".bright_cyan()
    );
    println!();
    println!("  Or download manually from:");
    println!("    {}", download_url.bright_blue());
    println!();
}

/// Print update check failed message
pub fn print_update_check_failed(error: &str) {
    println!(
        "  {} Failed to check for updates: {}",
        "✗".bright_red(),
        error
    );
    println!();
}

/// Print updating from GitHub message
pub fn print_updating_from_github() {
    println!();
    println!(
        "  {} Downloading latest version from GitHub...",
        "→".bright_blue()
    );
}

/// Print update failed message
pub fn print_update_failed(error: &str) {
    println!();
    println!("  {} Update failed: {}", "✗".bright_red(), error);
    println!();
}

/// Print pending update applied message (Windows)
pub fn print_pending_update_applied() {
    println!();
    println!(
        "  {} Pending update applied successfully.",
        "✓".bright_green()
    );
}

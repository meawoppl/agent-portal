//! Claude sign-in status for the launcher's agent triage matrix.
//!
//! Wraps claude-codes' `auth_status()` (`claude auth status --json`) and maps
//! it onto the agent-neutral [`shared::AgentLoginStatus`] the matrix renders,
//! so the launcher composes install state (from `session_lib::probe`) with
//! login state without either side knowing the other's shape.

use shared::AgentLoginStatus;

/// Read `claude`'s sign-in status from the CLI on `PATH`.
///
/// Any failure — binary absent, or output we can't parse — maps to `Unknown`,
/// never `LoggedOut`: "we couldn't tell" must not render as an actionable
/// logged-out cell (the install column already says whether the binary exists).
pub fn probe_login() -> AgentLoginStatus {
    match claude_codes::auth::auth_status() {
        Ok(status) if status.logged_in => AgentLoginStatus::LoggedIn {
            // Prefer the human email; fall back to the credential kind
            // (`claude.ai` / `console`) when the account has no email.
            label: status.email.or(status.auth_method),
            plan: status.subscription_type,
            via: None,
        },
        Ok(_) => AgentLoginStatus::LoggedOut,
        Err(_) => AgentLoginStatus::Unknown,
    }
}

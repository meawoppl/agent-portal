//! Codex sign-in status for the launcher's agent triage matrix.
//!
//! Hybrid, by design (see the agent-login contract): the sanctioned
//! `codex login status` subprocess answers the **liveness** question (exit 0 =
//! signed in — stored credentials are not necessarily live), and
//! codex-codes' `auth_status_local()` supplies the **display label** (email +
//! plan) by reading the CLI's own stored creds. The label is display-only and
//! degrades to absent on any read/parse failure; the crate owns the
//! private-file risk (fixture + live test there), so agent-portal never decodes
//! that file itself.

use shared::AgentLoginStatus;

/// Read `codex`'s sign-in status: `codex login status` for the live boolean,
/// enriched with the stored account label when signed in.
pub fn probe_login() -> AgentLoginStatus {
    // Liveness. A missing binary or spawn failure => Unknown (not "signed out").
    let live = match std::process::Command::new("codex")
        .args(["login", "status"])
        .output()
    {
        Ok(out) => out.status.success(),
        Err(_) => return AgentLoginStatus::Unknown,
    };
    if !live {
        return AgentLoginStatus::LoggedOut;
    }

    // Label: typed, crate-owned read of the stored creds. Any failure just
    // leaves the cell as an unlabeled "signed in".
    let (label, plan) = match codex_codes::auth_local::auth_status_local() {
        Ok(status) => match status.auth_mode.as_deref() {
            // API-key auth carries no account identity by design.
            Some("apikey") => (Some("API key".to_string()), None),
            // ChatGPT (or anything that recorded an email): show it + plan.
            _ => (status.email, status.plan_type),
        },
        Err(_) => (None, None),
    };

    AgentLoginStatus::LoggedIn {
        label,
        plan,
        via: None,
    }
}

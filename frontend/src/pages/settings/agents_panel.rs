//! Agents triage matrix (Settings ▸ Agents).
//!
//! A new user's first question is "what do I still need to set up, and where?".
//! This pane answers it as a **computer × agent** grid: one row per launcher
//! (host), one column per agent (Claude / Codex / Muse), each cell showing whether the
//! CLI is installed and whether it's signed in (+ the account label when the
//! agent exposes one). Data comes from the existing per-launcher probe
//! (`/api/launchers/{id}/probe-agents`), fanned across the user's launchers;
//! offline launchers render as unreachable rather than blank.
//!
//! A "signed out"/"unknown" cell for an installed agent gets a Sign-in button
//! that opens [`AgentLoginModal`], which drives the launcher-side login flow; a
//! successful sign-in re-probes the matrix.

use crate::pages::settings::agent_install::host_label;
use shared::{AgentInstall, AgentLoginStatus, AgentType, LauncherInfo};
use std::collections::HashMap;
use uuid::Uuid;
use yew::prelude::*;

/// Which cell's sign-in modal is open: (launcher, agent, agent display name).
#[derive(Clone, PartialEq)]
pub(super) struct LoginTarget {
    pub(super) launcher_id: Uuid,
    pub(super) agent_type: AgentType,
    pub(super) agent_name: String,
}

/// Which cell's install modal is open, plus the host label so the modal can
/// say *where* the install runs.
#[derive(Clone, PartialEq)]
pub(super) struct InstallTarget {
    pub(super) launcher_id: Uuid,
    pub(super) agent_type: AgentType,
    pub(super) agent_name: String,
    pub(super) host: String,
}

/// Columns of the matrix, in display order. Mirrors `AgentType`.
pub(super) const AGENTS: [(AgentType, &str); 3] = [
    (AgentType::Claude, "Claude"),
    (AgentType::Codex, "Codex"),
    (AgentType::Muse, "Muse"),
];

/// Per-launcher probe outcome. The whole map is set once, after every probe
/// resolves, so a launcher is either absent from the map (still loading, whole
/// pane shows "Loading…") or in one of these terminal states.
#[derive(Clone, PartialEq)]
pub(super) enum ProbeState {
    /// Launcher is offline (not connected) — can't be probed.
    Unreachable,
    /// Probe returned; agents keyed by type for O(1) cell lookup.
    Loaded(HashMap<AgentType, AgentInstall>),
}

pub(super) fn render_cell(
    state: Option<&ProbeState>,
    agent: AgentType,
    agent_name: &str,
    launcher: &LauncherInfo,
    on_sign_in: &Callback<LoginTarget>,
    on_install: &Callback<InstallTarget>,
) -> Html {
    match state {
        None => html! { <td class="agents-cell loading">{ "…" }</td> },
        Some(ProbeState::Unreachable) => {
            html! { <td class="agents-cell unreachable">{ "offline" }</td> }
        }
        Some(ProbeState::Loaded(agents)) => match agents.get(&agent) {
            Some(install) => {
                render_install_cell(install, agent, agent_name, launcher, on_sign_in, on_install)
            }
            // Probe ran but didn't report this agent at all — treat as unknown.
            None => html! { <td class="agents-cell unknown">{ "—" }</td> },
        },
    }
}

fn render_install_cell(
    install: &AgentInstall,
    agent: AgentType,
    agent_name: &str,
    launcher: &LauncherInfo,
    on_sign_in: &Callback<LoginTarget>,
    on_install: &Callback<InstallTarget>,
) -> Html {
    if !install.installed {
        let target = InstallTarget {
            launcher_id: launcher.launcher_id,
            agent_type: agent,
            agent_name: agent_name.to_string(),
            host: host_label(launcher),
        };
        let on_install = on_install.clone();
        let onclick = Callback::from(move |_: MouseEvent| on_install.emit(target.clone()));
        return html! {
            <td class="agents-cell not-installed">
                <span class="agents-badge missing">{ "not installed" }</span>
                <button class="agents-signin" {onclick}>{ "Install" }</button>
            </td>
        };
    }
    let (login_class, login_text) = login_summary(&install.login);
    html! {
        <td class="agents-cell installed">
            <span class="agents-badge installed">{ "installed" }</span>
            // The CSS ellipsizes long login labels; the tooltip carries the
            // full text.
            <span class={classes!("agents-login", login_class)} title={login_text.clone()}>
                { login_text }
            </span>
            { for sign_in_button(&install.login, agent, agent_name, launcher.launcher_id, on_sign_in) }
        </td>
    }
}

/// A Sign-in button, shown only when the agent is installed but not signed in
/// (or its state is unknown — offering the action can't hurt). `None` when
/// already signed in, so the option collapses out of the cell.
fn sign_in_button(
    login: &AgentLoginStatus,
    agent: AgentType,
    agent_name: &str,
    launcher_id: Uuid,
    on_sign_in: &Callback<LoginTarget>,
) -> Option<Html> {
    // Muse can be installed and its credential state is probed, but the
    // launcher-side interactive device-flow driver is not wired yet. Do not
    // offer a button that can only fail; host/env login remains visible after
    // Refresh and Claude/Codex retain the complete in-portal flow.
    if agent == AgentType::Muse || matches!(login, AgentLoginStatus::LoggedIn { .. }) {
        return None;
    }
    let target = LoginTarget {
        launcher_id,
        agent_type: agent,
        agent_name: agent_name.to_string(),
    };
    let on_sign_in = on_sign_in.clone();
    let onclick = Callback::from(move |_: MouseEvent| on_sign_in.emit(target.clone()));
    Some(html! {
        <button class="agents-signin" {onclick}>{ "Sign in" }</button>
    })
}

/// Cell text + CSS modifier for a login state. Pure, so the label precedence is
/// unit-tested without mounting the component.
fn login_summary(login: &AgentLoginStatus) -> (&'static str, String) {
    match login {
        AgentLoginStatus::Unknown => ("unknown", "sign-in unknown".to_string()),
        AgentLoginStatus::LoggedOut => ("logged-out", "signed out".to_string()),
        AgentLoginStatus::LoggedIn { label, plan, via } => {
            let mut text = match label {
                Some(l) => format!("signed in — {l}"),
                None => "signed in".to_string(),
            };
            if let Some(plan) = plan {
                text.push_str(&format!(" ({plan})"));
            }
            if let Some(via) = via {
                text.push_str(&format!(" [{via}]"));
            }
            ("logged-in", text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logged_in_with_email_and_plan_reads_naturally() {
        let (class, text) = login_summary(&AgentLoginStatus::LoggedIn {
            label: Some("matt@exclosure.io".to_string()),
            plan: Some("max".to_string()),
            via: None,
        });
        assert_eq!(class, "logged-in");
        assert_eq!(text, "signed in — matt@exclosure.io (max)");
    }

    #[test]
    fn logged_in_without_a_label_still_says_signed_in() {
        // muse's case: authenticated but no account identity.
        let (class, text) = login_summary(&AgentLoginStatus::LoggedIn {
            label: None,
            plan: None,
            via: Some("env".to_string()),
        });
        assert_eq!(class, "logged-in");
        assert_eq!(text, "signed in [env]");
    }

    #[test]
    fn unknown_is_distinct_from_signed_out() {
        // "couldn't tell" must never read as the actionable "signed out".
        assert_eq!(login_summary(&AgentLoginStatus::Unknown).0, "unknown");
        assert_eq!(login_summary(&AgentLoginStatus::LoggedOut).0, "logged-out");
        assert_ne!(
            login_summary(&AgentLoginStatus::Unknown).1,
            login_summary(&AgentLoginStatus::LoggedOut).1
        );
    }

    #[test]
    fn matrix_covers_every_agent_without_offering_broken_muse_login() {
        assert_eq!(AGENTS.len(), 3);
        assert!(AGENTS.iter().any(|(agent, _)| *agent == AgentType::Muse));
        assert!(sign_in_button(
            &AgentLoginStatus::LoggedOut,
            AgentType::Muse,
            "Muse",
            Uuid::nil(),
            &Callback::noop(),
        )
        .is_none());
    }
}

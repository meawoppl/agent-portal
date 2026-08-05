// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Agents triage matrix (Settings ▸ Agents).
//!
//! A new user's first question is "what do I still need to set up, and where?".
//! This pane answers it as a **computer × agent** grid: one row per launcher
//! (host), one column per agent (Claude / Codex), each cell showing whether the
//! CLI is installed and whether it's signed in (+ the account label when the
//! agent exposes one). Data comes from the existing per-launcher probe
//! (`/api/launchers/{id}/probe-agents`), fanned across the user's launchers;
//! offline launchers render as unreachable rather than blank.
//!
//! Read-only for now. The login buttons that act on a "signed out" cell land in
//! a follow-up against the rust-code-agent-sdks login flows.

use crate::utils::{self, On401};
use shared::api::ProbeAgentsResponse;
use shared::{AgentInstall, AgentLoginStatus, AgentType, LauncherInfo};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Columns of the matrix, in display order. Mirrors `AgentType`.
const AGENTS: [(AgentType, &str); 2] = [(AgentType::Claude, "Claude"), (AgentType::Codex, "Codex")];

/// Per-launcher probe outcome. The whole map is set once, after every probe
/// resolves, so a launcher is either absent from the map (still loading, whole
/// pane shows "Loading…") or in one of these terminal states.
#[derive(Clone, PartialEq)]
enum ProbeState {
    /// Launcher is offline (not connected) — can't be probed.
    Unreachable,
    /// Probe returned; agents keyed by type for O(1) cell lookup.
    Loaded(HashMap<AgentType, AgentInstall>),
}

#[function_component(AgentsPanel)]
pub fn agents_panel() -> Html {
    let launchers = use_state(|| None::<Vec<LauncherInfo>>);
    let probes = use_state(HashMap::<Uuid, ProbeState>::new);
    // Bumped by the refresh button to re-run the whole fan-out.
    let refresh = use_state(|| 0u32);

    {
        let launchers = launchers.clone();
        let probes = probes.clone();
        use_effect_with(*refresh, move |_| {
            launchers.set(None);
            probes.set(HashMap::new());
            spawn_local(async move {
                let list = utils::fetch_json::<Vec<LauncherInfo>>("/api/launchers", On401::Ignore)
                    .await
                    .unwrap_or_default();

                // Probe sequentially — a settings pane has a handful of hosts,
                // and this keeps the state update race-free (one set at the end)
                // without pulling in a join_all.
                let mut collected: HashMap<Uuid, ProbeState> = HashMap::new();
                for l in &list {
                    if !l.connected {
                        collected.insert(l.launcher_id, ProbeState::Unreachable);
                        continue;
                    }
                    let path = format!("/api/launchers/{}/probe-agents", l.launcher_id);
                    let state = match utils::fetch_json::<ProbeAgentsResponse>(&path, On401::Ignore)
                        .await
                    {
                        Ok(resp) => ProbeState::Loaded(
                            resp.agents.into_iter().map(|a| (a.agent_type, a)).collect(),
                        ),
                        // A connected launcher that fails to answer (dropped
                        // mid-probe, timeout) is unreachable for our purposes.
                        Err(_) => ProbeState::Unreachable,
                    };
                    collected.insert(l.launcher_id, state);
                }

                launchers.set(Some(list));
                probes.set(collected);
            });
            || ()
        });
    }

    let on_refresh = {
        let refresh = refresh.clone();
        Callback::from(move |_: MouseEvent| refresh.set(*refresh + 1))
    };

    let body = match (*launchers).clone() {
        None => html! { <p class="setting-description">{ "Loading…" }</p> },
        Some(list) if list.is_empty() => html! {
            <p class="setting-description">
                { "No computers connected yet. Install the agent-portal launcher on a \
                   machine and it'll appear here." }
            </p>
        },
        Some(list) => html! {
            <table class="agents-matrix">
                <thead>
                    <tr>
                        <th>{ "Computer" }</th>
                        { for AGENTS.iter().map(|(_, name)| html! { <th>{ *name }</th> }) }
                    </tr>
                </thead>
                <tbody>
                    { for list.iter().map(|l| render_row(l, &probes)) }
                </tbody>
            </table>
        },
    };

    html! {
        <section class="agents-section">
            <div class="section-header">
                <h2>{ "Agents" }</h2>
                <p class="section-description">
                    { "Install and sign-in state for each agent on every connected \
                       computer. " }
                    <button class="link-button" onclick={on_refresh}>{ "Refresh" }</button>
                </p>
            </div>
            { body }
        </section>
    }
}

fn render_row(launcher: &LauncherInfo, probes: &HashMap<Uuid, ProbeState>) -> Html {
    let state = probes.get(&launcher.launcher_id);
    html! {
        <tr>
            <td class="agents-host">
                <span class="agents-host-name">{ &launcher.hostname }</span>
                if launcher.launcher_name != launcher.hostname {
                    <span class="agents-host-alias">{ format!("({})", launcher.launcher_name) }</span>
                }
            </td>
            { for AGENTS.iter().map(|(agent, _)| render_cell(state, *agent)) }
        </tr>
    }
}

fn render_cell(state: Option<&ProbeState>, agent: AgentType) -> Html {
    match state {
        None => html! { <td class="agents-cell loading">{ "…" }</td> },
        Some(ProbeState::Unreachable) => {
            html! { <td class="agents-cell unreachable">{ "offline" }</td> }
        }
        Some(ProbeState::Loaded(agents)) => match agents.get(&agent) {
            Some(install) => render_install_cell(install),
            // Probe ran but didn't report this agent at all — treat as unknown.
            None => html! { <td class="agents-cell unknown">{ "—" }</td> },
        },
    }
}

fn render_install_cell(install: &AgentInstall) -> Html {
    if !install.installed {
        return html! {
            <td class="agents-cell not-installed">
                <span class="agents-badge missing">{ "not installed" }</span>
            </td>
        };
    }
    let (login_class, login_text) = login_summary(&install.login);
    html! {
        <td class="agents-cell installed">
            <span class="agents-badge installed">{ "installed" }</span>
            <span class={classes!("agents-login", login_class)}>{ login_text }</span>
        </td>
    }
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
}

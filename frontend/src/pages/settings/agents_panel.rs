// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

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

use crate::pages::settings::agent_install::{host_label, AgentInstallModal};
use crate::pages::settings::agent_login::AgentLoginModal;
use crate::utils::{self, On401};
use shared::api::ProbeAgentsResponse;
use shared::{AgentInstall, AgentLoginStatus, AgentType, LauncherInfo};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Which cell's sign-in modal is open: (launcher, agent, agent display name).
#[derive(Clone, PartialEq)]
struct LoginTarget {
    launcher_id: Uuid,
    agent_type: AgentType,
    agent_name: String,
}

/// Which cell's install modal is open, plus the host label so the modal can
/// say *where* the install runs.
#[derive(Clone, PartialEq)]
struct InstallTarget {
    launcher_id: Uuid,
    agent_type: AgentType,
    agent_name: String,
    host: String,
}

/// Columns of the matrix, in display order. Mirrors `AgentType`.
const AGENTS: [(AgentType, &str); 3] = [
    (AgentType::Claude, "Claude"),
    (AgentType::Codex, "Codex"),
    (AgentType::Muse, "Muse"),
];

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
    // Bumped by the refresh button (and a successful sign-in) to re-run the fan-out.
    let refresh = use_state(|| 0u32);
    // The open sign-in modal, if any.
    let login_target = use_state(|| None::<LoginTarget>);
    // The open install modal, if any.
    let install_target = use_state(|| None::<InstallTarget>);

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

    let on_sign_in = {
        let login_target = login_target.clone();
        Callback::from(move |target: LoginTarget| login_target.set(Some(target)))
    };

    let on_install = {
        let install_target = install_target.clone();
        Callback::from(move |target: InstallTarget| install_target.set(Some(target)))
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
                    { for list.iter().map(|l| render_row(l, &probes, &on_sign_in, &on_install)) }
                </tbody>
            </table>
        },
    };

    let login_modal = (*login_target).clone().map(|target| {
        let on_close = {
            let login_target = login_target.clone();
            Callback::from(move |_| login_target.set(None))
        };
        let on_success = {
            let refresh = refresh.clone();
            Callback::from(move |_| refresh.set(*refresh + 1))
        };
        html! {
            <AgentLoginModal
                launcher_id={target.launcher_id}
                agent_type={target.agent_type}
                agent_name={target.agent_name}
                {on_close}
                {on_success}
            />
        }
    });

    let install_modal = (*install_target).clone().map(|target| {
        let on_close = {
            let install_target = install_target.clone();
            Callback::from(move |_| install_target.set(None))
        };
        let on_success = {
            let refresh = refresh.clone();
            Callback::from(move |_| refresh.set(*refresh + 1))
        };
        html! {
            <AgentInstallModal
                launcher_id={target.launcher_id}
                agent_type={target.agent_type}
                agent_name={target.agent_name}
                host={target.host}
                {on_close}
                {on_success}
            />
        }
    });

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
            { for login_modal }
            { for install_modal }
        </section>
    }
}

fn render_row(
    launcher: &LauncherInfo,
    probes: &HashMap<Uuid, ProbeState>,
    on_sign_in: &Callback<LoginTarget>,
    on_install: &Callback<InstallTarget>,
) -> Html {
    let state = probes.get(&launcher.launcher_id);
    html! {
        <tr>
            <td class="agents-host">
                <span class="agents-host-name">{ &launcher.hostname }</span>
                if launcher.launcher_name != launcher.hostname {
                    <span class="agents-host-alias">{ format!("({})", launcher.launcher_name) }</span>
                }
            </td>
            { for AGENTS.iter().map(|(agent, name)| {
                render_cell(state, *agent, name, launcher, on_sign_in, on_install)
            }) }
        </tr>
    }
}

fn render_cell(
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
            <span class={classes!("agents-login", login_class)}>{ login_text }</span>
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

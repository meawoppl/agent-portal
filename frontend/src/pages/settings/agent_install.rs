//! Agent install-helper modal (Settings ▸ Agents).
//!
//! A not-installed cell offers an Install button that opens this modal. It
//! shows the exact command that will run on the launcher host (from
//! [`AgentType::install_command`], so the user can see it's a global npm
//! install — or, for agents whose only installer is a piped script, exactly
//! that), waits for confirmation, then POSTs to the install endpoint. On
//! success it fires `on_success` so the matrix re-probes.

use gloo_net::http::Request;
use shared::api::InstallAgentResponse;
use shared::{AgentType, LauncherInfo};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::utils;

#[derive(Properties, PartialEq)]
pub struct AgentInstallModalProps {
    pub launcher_id: Uuid,
    pub agent_type: AgentType,
    /// Human label for the agent (e.g. "Codex"), for the modal title.
    pub agent_name: String,
    /// Host label, so the user knows *where* the install runs.
    pub host: String,
    pub on_close: Callback<()>,
    /// Fired once when the install succeeds, so the caller can re-probe.
    pub on_success: Callback<()>,
}

#[derive(Clone, PartialEq)]
enum Stage {
    /// Showing the command, waiting for the user to confirm.
    Confirm,
    /// `POST /install` in flight.
    Running,
    /// Terminal outcome (success or a reported failure).
    Done(InstallAgentResponse),
}

#[function_component(AgentInstallModal)]
pub fn agent_install_modal(props: &AgentInstallModalProps) -> Html {
    let stage = use_state(|| Stage::Confirm);
    let command = props.agent_type.install_command().display();

    let on_install = {
        let stage = stage.clone();
        let launcher_id = props.launcher_id;
        let agent_type = props.agent_type;
        let on_success = props.on_success.clone();
        Callback::from(move |_: MouseEvent| {
            stage.set(Stage::Running);
            let stage = stage.clone();
            let on_success = on_success.clone();
            spawn_local(async move {
                let outcome = install_agent(launcher_id, agent_type).await;
                let resp = match outcome {
                    Ok(resp) => resp,
                    Err(message) => InstallAgentResponse {
                        success: false,
                        message: Some(message),
                    },
                };
                if resp.success {
                    on_success.emit(());
                }
                stage.set(Stage::Done(resp));
            });
        })
    };

    let on_close = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };
    let stop = Callback::from(|e: MouseEvent| e.stop_propagation());

    let body = match &*stage {
        Stage::Confirm => html! {
            <>
                <p class="agent-login-instr">
                    { format!("This runs the following on {}:", props.host) }
                </p>
                <code class="agent-install-command">{ &command }</code>
                <button class="agent-login-submit" onclick={on_install}>{ "Install" }</button>
            </>
        },
        Stage::Running => html! {
            <p class="agent-login-status">{ format!("Installing {}…", props.agent_name) }</p>
        },
        Stage::Done(resp) => {
            let (class, text) = install_status(resp);
            html! { <p class={classes!("agent-login-status", class)}>{ text }</p> }
        }
    };

    let close_label = if matches!(&*stage, Stage::Done(_)) {
        "Close"
    } else {
        "Cancel"
    };

    html! {
        <div class="agent-login-overlay" onclick={on_close.clone()}>
            <div class="agent-login-modal" onclick={stop}>
                <div class="agent-login-header">
                    <h2>{ format!("Install {}", props.agent_name) }</h2>
                    <button class="agent-login-close" onclick={on_close.clone()}>{ "×" }</button>
                </div>
                <div class="agent-login-body">{ body }</div>
                <div class="agent-login-footer">
                    <button class="link-button" onclick={on_close}>{ close_label }</button>
                </div>
            </div>
        </div>
    }
}

/// Host label for the modal: hostname, or the launcher alias when it differs.
pub fn host_label(launcher: &LauncherInfo) -> String {
    if launcher.launcher_name != launcher.hostname {
        format!("{} ({})", launcher.hostname, launcher.launcher_name)
    } else {
        launcher.hostname.clone()
    }
}

/// CSS modifier + text for a settled install. Pure, so the wording is
/// unit-tested without mounting the component.
fn install_status(resp: &InstallAgentResponse) -> (&'static str, String) {
    if resp.success {
        ("success", "Installed. Refreshing…".to_string())
    } else {
        let detail = resp
            .message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("the install command failed");
        ("error", format!("Install failed: {detail}"))
    }
}

async fn install_agent(
    launcher_id: Uuid,
    agent_type: AgentType,
) -> Result<InstallAgentResponse, String> {
    let url = utils::api_url(&format!(
        "/api/launchers/{launcher_id}/agents/{}/install",
        agent_type.as_str()
    ));
    let resp = Request::post(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(if body.trim().is_empty() {
            format!("HTTP {status}")
        } else {
            body
        });
    }
    resp.json::<InstallAgentResponse>()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_reads_cleanly() {
        let (class, text) = install_status(&InstallAgentResponse {
            success: true,
            message: None,
        });
        assert_eq!(class, "success");
        assert_eq!(text, "Installed. Refreshing…");
    }

    #[test]
    fn failure_surfaces_the_command_output() {
        let (class, text) = install_status(&InstallAgentResponse {
            success: false,
            message: Some("npm ERR! code EACCES".to_string()),
        });
        assert_eq!(class, "error");
        assert_eq!(text, "Install failed: npm ERR! code EACCES");
    }

    #[test]
    fn failure_without_a_message_has_a_fallback() {
        let (_, text) = install_status(&InstallAgentResponse {
            success: false,
            message: None,
        });
        assert_eq!(text, "Install failed: the install command failed");
    }

    #[test]
    fn command_line_is_the_documented_installer() {
        assert_eq!(
            AgentType::Codex.install_command().display(),
            "npm install -g @openai/codex"
        );
        assert_eq!(
            AgentType::Claude.install_command().display(),
            "npm install -g @anthropic-ai/claude-code"
        );
    }
}

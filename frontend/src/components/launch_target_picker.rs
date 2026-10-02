//! Shared host, agent, and directory selectors for interactive and scheduled
//! launches. Keeping the controls in one component prevents the scheduler from
//! drifting back to free-form host/path fields when launcher behavior changes.

use shared::{AgentInstall, AgentType, LauncherInfo};
use uuid::Uuid;
use yew::prelude::*;

pub const CONNECT_NEW: &str = "__install__";

fn agent_installed(installs: &[AgentInstall], agent_type: AgentType) -> Option<bool> {
    installs
        .iter()
        .find(|install| install.agent_type == agent_type)
        .map(|install| install.installed)
}

#[derive(Properties, PartialEq)]
pub struct LauncherTargetSelectProps {
    pub launchers: Vec<LauncherInfo>,
    pub selected: Option<Uuid>,
    pub on_change: Callback<Event>,
    #[prop_or(false)]
    pub include_connect_new: bool,
    #[prop_or(false)]
    pub connect_new_selected: bool,
    #[prop_or(false)]
    pub show_hostname: bool,
    #[prop_or(false)]
    pub show_version: bool,
    #[prop_or_default]
    pub offline_hostname: Option<String>,
}

#[function_component(LauncherTargetSelect)]
pub fn launcher_target_select(props: &LauncherTargetSelectProps) -> Html {
    let selected_info = props.selected.and_then(|id| {
        props
            .launchers
            .iter()
            .find(|launcher| launcher.launcher_id == id)
    });

    html! {
        <div class="launch-field">
            <label>{ "Host" }</label>
            <select class="launcher-select" onchange={props.on_change.clone()}>
                if props.selected.is_none() {
                    if let Some(hostname) = &props.offline_hostname {
                        <option value="" selected=true>{ format!("{hostname} (offline)") }</option>
                    }
                }
                { for props.launchers.iter().map(|launcher| {
                    let label = if props.show_hostname {
                        format!("{} — {}", launcher.launcher_name, launcher.hostname)
                    } else {
                        launcher.launcher_name.clone()
                    };
                    html! {
                        <option value={launcher.launcher_id.to_string()} selected={props.selected == Some(launcher.launcher_id)}>
                            { label }
                        </option>
                    }
                }) }
                if props.include_connect_new && !props.launchers.is_empty() {
                    <option disabled=true value="">{ "──────────────" }</option>
                }
                if props.include_connect_new {
                    <option value={CONNECT_NEW} selected={props.connect_new_selected}>{ "+ Connect New Host" }</option>
                }
            </select>
            if let Some(info) = selected_info {
                <span class="launcher-subtitle">
                    { format!(
                        "{} running{}",
                        info.running_sessions,
                        if props.show_version && !info.version.is_empty() {
                            format!(" · v{}", info.version)
                        } else {
                            String::new()
                        },
                    ) }
                </span>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct AgentTargetSelectProps {
    pub selected: AgentType,
    pub installs: Vec<AgentInstall>,
    pub probing: bool,
    pub on_change: Callback<Event>,
}

#[function_component(AgentTargetSelect)]
pub fn agent_target_select(props: &AgentTargetSelectProps) -> Html {
    const AGENTS: [AgentType; 4] = [
        AgentType::Claude,
        AgentType::Codex,
        AgentType::Muse,
        AgentType::Antigravity,
    ];
    let label = |agent: AgentType| match agent_installed(&props.installs, agent) {
        Some(false) => format!("{} (not installed)", agent.display_name()),
        _ => agent.display_name().to_string(),
    };
    let missing = agent_installed(&props.installs, props.selected) == Some(false);

    html! {
        <>
            <div class="launch-field">
                <label>{ "Agent" }</label>
                <select class="launcher-select" onchange={props.on_change.clone()}>
                    { for AGENTS.iter().map(|agent| html! {
                        <option value={agent.as_str()} selected={props.selected == *agent}>{ label(*agent) }</option>
                    }) }
                </select>
            </div>
            if missing {
                <div class="launch-note launch-note-warn">
                    { format!("{} isn't installed on this host — runs will fail to start.", props.selected.display_name()) }
                </div>
            } else if props.probing && props.installs.is_empty() {
                <div class="launch-note">{ "Checking installed agents..." }</div>
            }
            if props.selected == AgentType::Muse {
                <div class="launch-note launch-note-warn">{ "Muse support is highly experimental." }</div>
            }
            if props.selected == AgentType::Antigravity {
                <div class="launch-note launch-note-warn">
                    { "Antigravity support is a read-only preview. It requires GEMINI_API_KEY or Vertex ADC on the launcher host." }
                </div>
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
pub struct DirectoryTargetSelectProps {
    pub path: String,
    pub breadcrumbs: Vec<(String, String)>,
    pub listing: Html,
    pub on_path_input: Callback<InputEvent>,
    pub on_path_keydown: Callback<KeyboardEvent>,
    pub on_navigate: Callback<String>,
    #[prop_or_default]
    pub browser_class: Classes,
}

#[function_component(DirectoryTargetSelect)]
pub fn directory_target_select(props: &DirectoryTargetSelectProps) -> Html {
    html! {
        <div class="launch-field">
            <label>{ "Directory (home folder only)" }</label>
            <input
                type="text"
                class="dir-path-input"
                placeholder="~/project"
                value={props.path.clone()}
                oninput={props.on_path_input.clone()}
                onkeydown={props.on_path_keydown.clone()}
                required=true
            />
            <div class="dir-breadcrumb">
                { for props.breadcrumbs.iter().enumerate().map(|(index, (full_path, label))| {
                    let path = full_path.clone();
                    let is_last = index + 1 == props.breadcrumbs.len();
                    let on_navigate = props.on_navigate.clone();
                    let onclick = Callback::from(move |event: MouseEvent| {
                        event.prevent_default();
                        on_navigate.emit(path.clone());
                    });
                    html! {
                        <>
                            if index > 0 { <span class="dir-breadcrumb-sep">{ "/" }</span> }
                            <a class={classes!("dir-breadcrumb-seg", is_last.then_some("active"))} href="#" {onclick}>{ label }</a>
                        </>
                    }
                }) }
            </div>
            <div class={classes!("dir-browser", props.browser_class.clone())}>{ props.listing.clone() }</div>
        </div>
    }
}

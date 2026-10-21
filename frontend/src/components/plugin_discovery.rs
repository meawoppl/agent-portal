use shared::api::{
    PluginAction, PluginInventoryResponse, PluginPolicy, PluginRequest, PluginResponse,
    PortalPluginInfo,
};
use std::collections::BTreeMap;
use uuid::Uuid;
use yew::prelude::*;

use crate::utils;

/// The top-level Plugins browser.
pub const PLUGINS_BROWSER: &str = "/settings?tab=plugins";

/// Run one named plugin action on a computer.
pub async fn run_plugin_action(
    launcher_id: Uuid,
    request: &PluginRequest,
) -> Result<PluginResponse, String> {
    let url = utils::api_url(&format!("/api/launchers/{launcher_id}/plugins"));
    let response = utils::send_json(gloo_net::http::Request::post(&url), request)
        .await
        .map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(utils::error_body(response).await);
    }
    response
        .json::<PluginResponse>()
        .await
        .map_err(|e| e.to_string())
}

/// A computer's installed plugins; with a directory, matched against it.
pub async fn fetch_launcher_plugins(
    launcher_id: Uuid,
    working_directory: Option<String>,
) -> Result<PluginInventoryResponse, String> {
    let request = PluginRequest {
        working_directory: working_directory.filter(|path| shared::strings::is_non_blank(path)),
        session_id: None,
        action: PluginAction::Inventory,
    };
    let response = run_plugin_action(launcher_id, &request).await?;
    response
        .inventory
        .ok_or_else(|| "The launcher returned no plugin inventory".to_string())
}

pub fn suggested_plugins(plugins: &[PortalPluginInfo]) -> Vec<&PortalPluginInfo> {
    plugins
        .iter()
        .filter(|plugin| plugin.active || plugin.suggested)
        .collect()
}

pub fn plugin_context_label(bytes: u64, tokens: u64) -> String {
    if bytes == 0 {
        return "no skill context measured".to_string();
    }
    let kib = bytes as f64 / 1024.0;
    if kib >= 10.0 {
        format!("{kib:.0} KiB · ~{tokens} tokens")
    } else {
        format!("{kib:.1} KiB · ~{tokens} tokens")
    }
}

/// The launch-dialog choices that differ from what the session would load
/// anyway. `None` when there are none, so the launcher leaves every plugin to
/// policy and detection.
pub fn plugin_overrides(
    plugins: &[PortalPluginInfo],
    choices: &BTreeMap<String, bool>,
) -> Option<BTreeMap<String, bool>> {
    let changed: BTreeMap<String, bool> = plugins
        .iter()
        .filter_map(|plugin| {
            let enabled = *choices.get(&plugin.name)?;
            (enabled != plugin.enabled).then(|| (plugin.name.clone(), enabled))
        })
        .collect();
    (!changed.is_empty()).then_some(changed)
}

/// Why a plugin is on or off for a directory, for its tooltip.
pub fn plugin_state_reason(plugin: &PortalPluginInfo) -> String {
    match (
        plugin.policy,
        plugin.disabled_by_policy,
        plugin.reason.as_deref(),
    ) {
        (PluginPolicy::Always, _, _) => "Always on in this project".to_string(),
        (PluginPolicy::Never, true, Some(reason)) => {
            format!("{reason}, but never loaded in this project")
        }
        (PluginPolicy::Never, _, _) => "Never loaded in this project".to_string(),
        (PluginPolicy::Ask, _, Some(reason)) => reason.to_string(),
        (PluginPolicy::Ask, _, None) => "Not detected in this directory".to_string(),
    }
}

#[derive(Properties, PartialEq)]
pub struct PluginSuggestionStripProps {
    pub plugins: Vec<PortalPluginInfo>,
    #[prop_or_default]
    pub error: Option<String>,
    /// Plugins switched on or off before launch.
    pub choices: BTreeMap<String, bool>,
    pub on_toggle: Callback<(String, bool)>,
}

/// The launch dialog's plugins: on where detected or required by the
/// project's policy, and switchable for the session about to start.
#[function_component(PluginSuggestionStrip)]
pub fn plugin_suggestion_strip(props: &PluginSuggestionStripProps) -> Html {
    let browse = html! {
        <a class="plugin-browse-link" href={PLUGINS_BROWSER} target="_blank" rel="noopener noreferrer">{ "Browse plugins" }</a>
    };
    if let Some(error) = props.error.as_deref() {
        return html! {
            <div class="plugin-suggestion-strip">
                <span class="plugin-suggestion-title">{ "Plugins" }</span>
                <span class="plugin-suggestion-error">{ error }</span>
                { browse }
            </div>
        };
    }
    if props.plugins.is_empty() {
        return html! {
            <div class="plugin-suggestion-strip muted">
                <span class="plugin-suggestion-title">{ "Plugins" }</span>
                <span>{ "None installed on this computer" }</span>
                { browse }
            </div>
        };
    }
    let mut plugins: Vec<&PortalPluginInfo> = props.plugins.iter().collect();
    let is_on = |plugin: &PortalPluginInfo| {
        props
            .choices
            .get(&plugin.name)
            .copied()
            .unwrap_or(plugin.enabled)
    };
    plugins.sort_by_key(|plugin| (!plugin.enabled, !plugin.suggested));
    html! {
        <div class="plugin-suggestion-strip">
            <span class="plugin-suggestion-title">{ "Plugins" }</span>
            <span class="plugin-suggestion-list">
                { for plugins.into_iter().map(|plugin| {
                    let on = is_on(plugin);
                    let changed = on != plugin.enabled;
                    let title = format!(
                        "{}{}",
                        plugin_state_reason(plugin),
                        if changed { " · changed for this session" } else { "" }
                    );
                    let onclick = {
                        let on_toggle = props.on_toggle.clone();
                        let name = plugin.name.clone();
                        Callback::from(move |_| on_toggle.emit((name.clone(), !on)))
                    };
                    html! {
                        <button
                            type="button"
                            class={classes!(
                                "plugin-suggestion-pill",
                                on.then_some("on"),
                                changed.then_some("changed"),
                            )}
                            aria-pressed={on.to_string()}
                            {title}
                            {onclick}
                        >
                            { &plugin.display_name }
                            if plugin.policy != PluginPolicy::Ask {
                                <span class="plugin-policy-mark">{ plugin_policy_label(plugin.policy) }</span>
                            }
                        </button>
                    }
                }) }
            </span>
            { browse }
        </div>
    }
}

pub fn plugin_policy_label(policy: PluginPolicy) -> &'static str {
    match policy {
        PluginPolicy::Ask => "ask",
        PluginPolicy::Always => "always",
        PluginPolicy::Never => "never",
    }
}

fn plugin_file_pills(files: &[shared::api::PortalPluginSkillInfo], class: &'static str) -> Html {
    html! {
        <div class="plugin-skill-list">
            { for files.iter().map(|file| {
                let title = file
                    .description
                    .clone()
                    .unwrap_or_else(|| file.path.clone());
                html! { <span class={class} title={title} key={file.name.clone()}>{ &file.name }</span> }
            }) }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct PluginCardProps {
    pub plugin: PortalPluginInfo,
    /// Controls rendered at the foot of the card.
    #[prop_or_default]
    pub children: Html,
}

/// One installed plugin: what it is, where it lives, and what it contributes.
#[function_component(PluginCard)]
pub fn plugin_card(props: &PluginCardProps) -> Html {
    let plugin = &props.plugin;
    let is_suggested = plugin.active || plugin.suggested;
    let agents = if plugin.compatibility.is_empty() {
        "All agents".to_string()
    } else {
        plugin.compatibility.join(", ")
    };
    html! {
        <article class={classes!("plugin-card", is_suggested.then_some("suggested"))} key={plugin.name.clone()}>
            <div class="plugin-card-topline">
                <div class="plugin-card-title">
                    <span>{ &plugin.display_name }</span>
                    if is_suggested {
                        <span class="plugin-card-badge">{ "suggested" }</span>
                    }
                    if plugin.disabled_by_policy {
                        <span class="plugin-card-badge muted" title={plugin_state_reason(plugin)}>{ "off by policy" }</span>
                    } else if !plugin.enabled {
                        <span class="plugin-card-badge muted">{ "off" }</span>
                    }
                    if let Some(version) = plugin.version.as_deref() {
                        <span class="plugin-card-version">{ version }</span>
                    }
                    if plugin.update_available == Some(true) {
                        <span class="plugin-card-badge update">{ "update available" }</span>
                    }
                </div>
                <span class="plugin-card-context">
                    { plugin_context_label(plugin.context_bytes, plugin.estimated_tokens) }
                </span>
            </div>
            if let Some(description) = plugin.description.as_deref() {
                <p class="plugin-card-description">{ description }</p>
            }
            if let Some(reason) = plugin.reason.as_deref() {
                <div class="plugin-card-reason">{ reason }</div>
            }
            if plugin.policy != PluginPolicy::Ask {
                <div class="plugin-card-policy">{ plugin_state_reason(plugin) }</div>
            }
            <dl class="plugin-card-facts">
                <dt>{ "Agents" }</dt>
                <dd>{ agents }</dd>
                if let Some(kind) = plugin.surface_kind.as_deref() {
                    <dt>{ "Surface" }</dt>
                    <dd>{ plugin.surface_title.clone().map(|title| format!("{title} ({kind})")).unwrap_or_else(|| kind.to_string()) }</dd>
                }
                if let Some(source) = plugin.source.as_deref() {
                    <dt>{ "Source" }</dt>
                    <dd><code class="plugin-card-source">{ source }</code></dd>
                }
                if let Some(path) = plugin.path.as_deref() {
                    <dt>{ "Path" }</dt>
                    <dd><code class="plugin-card-source">{ path }</code></dd>
                }
                if let Some(homepage) = plugin.homepage.as_deref() {
                    <dt>{ "Docs" }</dt>
                    <dd><a href={homepage.to_string()} target="_blank" rel="noopener noreferrer">{ homepage }</a></dd>
                }
            </dl>
            if !plugin.skills.is_empty() {
                <div class="plugin-card-section">
                    <span class="plugin-card-section-title">{ "Skills" }</span>
                    { plugin_file_pills(&plugin.skills, "plugin-skill-pill") }
                </div>
            }
            if !plugin.prompts.is_empty() {
                <div class="plugin-card-section">
                    <span class="plugin-card-section-title">{ "Prompts" }</span>
                    { plugin_file_pills(&plugin.prompts, "plugin-skill-pill") }
                </div>
            }
            if !plugin.commands.is_empty() {
                <div class="plugin-card-section">
                    <span class="plugin-card-section-title">{ "Commands" }</span>
                    <div class="plugin-skill-list">
                        { for plugin.commands.iter().map(|command| {
                            let title = command
                                .description
                                .clone()
                                .unwrap_or_else(|| "Plugin command".to_string());
                            html! {
                                <span class="plugin-command-pill" title={title} key={command.name.clone()}>
                                    { &command.name }
                                </span>
                            }
                        }) }
                    </div>
                </div>
            }
            if !plugin.warnings.is_empty() {
                <ul class="plugin-card-warnings">
                    { for plugin.warnings.iter().map(|warning| html! { <li>{ warning }</li> }) }
                </ul>
            }
            { props.children.clone() }
        </article>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str, enabled: bool) -> PortalPluginInfo {
        PortalPluginInfo {
            name: name.into(),
            display_name: name.into(),
            enabled,
            ..Default::default()
        }
    }

    fn choices(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
        pairs.iter().map(|(n, e)| (n.to_string(), *e)).collect()
    }

    /// Only real changes are sent; with none, the launcher leaves every plugin
    /// to policy and detection.
    #[test]
    fn overrides_carry_only_real_changes() {
        let plugins = [plugin("kicadmium", true), plugin("visilog", false)];
        assert_eq!(plugin_overrides(&plugins, &BTreeMap::new()), None);
        // Toggled away and back: the choice equals the effective state.
        assert_eq!(
            plugin_overrides(&plugins, &choices(&[("kicadmium", true)])),
            None
        );
        assert_eq!(
            plugin_overrides(
                &plugins,
                &choices(&[("kicadmium", false), ("visilog", true)])
            ),
            Some(choices(&[("kicadmium", false), ("visilog", true)]))
        );
        // A name no longer in the inventory is dropped, not sent.
        assert_eq!(
            plugin_overrides(&plugins, &choices(&[("removed", true)])),
            None
        );
    }
}

use shared::api::{
    PluginAction, PluginInventoryResponse, PluginRequest, PluginResponse, PortalPluginInfo,
};
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

#[derive(Properties, PartialEq)]
pub struct PluginSuggestionStripProps {
    pub plugins: Vec<PortalPluginInfo>,
    #[prop_or_default]
    pub error: Option<String>,
}

#[function_component(PluginSuggestionStrip)]
pub fn plugin_suggestion_strip(props: &PluginSuggestionStripProps) -> Html {
    let suggested = suggested_plugins(&props.plugins);
    if suggested.is_empty() && props.error.is_none() {
        return html! {
            <div class="plugin-suggestion-strip muted">
                <span class="plugin-suggestion-title">{ "Plugins" }</span>
                <span>{ "No directory-specific suggestions yet" }</span>
                <a class="plugin-browse-link" href={PLUGINS_BROWSER} target="_blank" rel="noopener noreferrer">{ "Browse plugins" }</a>
            </div>
        };
    }

    html! {
        <div class="plugin-suggestion-strip">
            <span class="plugin-suggestion-title">{ "Suggested plugins" }</span>
            if let Some(error) = props.error.as_deref() {
                <span class="plugin-suggestion-error">{ error }</span>
            } else {
                <span class="plugin-suggestion-list">
                    { for suggested.into_iter().map(|plugin| {
                        let title = plugin
                            .reason
                            .clone()
                            .unwrap_or_else(|| "Suggested for this directory".to_string());
                        html! {
                            <span class="plugin-suggestion-pill" title={title}>
                                { &plugin.display_name }
                            </span>
                        }
                    }) }
                </span>
            }
            <a class="plugin-browse-link" href={PLUGINS_BROWSER} target="_blank" rel="noopener noreferrer">{ "Browse plugins" }</a>
        </div>
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
                    if !plugin.enabled {
                        <span class="plugin-card-badge muted">{ "off" }</span>
                    }
                    if let Some(version) = plugin.version.as_deref() {
                        <span class="plugin-card-version">{ version }</span>
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
        </article>
    }
}

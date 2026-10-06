use shared::api::{PluginOverride, PluginPolicy, PortalPluginInfo};
use uuid::Uuid;
use yew::prelude::*;

pub fn plugin_inventory_api_path(
    launcher_id: Option<Uuid>,
    session_id: Option<Uuid>,
    working_directory: Option<&str>,
) -> String {
    let mut query = Vec::new();
    if let Some(id) = launcher_id {
        query.push(format!("launcherId={id}"));
    }
    if let Some(session_id) = session_id {
        query.push(format!("sessionId={session_id}"));
    }
    if let Some(path) = working_directory.filter(|path| shared::strings::is_non_blank(path)) {
        query.push(format!(
            "workingDirectory={}",
            js_sys::encode_uri_component(path)
        ));
    }
    if query.is_empty() {
        "/api/plugins".to_string()
    } else {
        format!("/api/plugins?{}", query.join("&"))
    }
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
    pub overrides: Vec<PluginOverride>,
    pub on_override: Callback<PluginOverride>,
    #[prop_or_default]
    pub error: Option<String>,
}

#[function_component(PluginSuggestionStrip)]
pub fn plugin_suggestion_strip(props: &PluginSuggestionStripProps) -> Html {
    let suggested = &props.plugins;
    if suggested.is_empty() && props.error.is_none() {
        return html! {
            <div class="plugin-suggestion-strip muted">
                <span class="plugin-suggestion-title">{ "Plugins" }</span>
                <span>{ "No directory-specific suggestions yet" }</span>
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
                    { for suggested.iter().map(|plugin| {
                        let title = plugin
                            .reason
                            .clone()
                            .unwrap_or_else(|| "Suggested for this directory".to_string());
                        let checked = props.overrides.iter().find(|item| item.name == plugin.name)
                            .map(|item| item.enabled).unwrap_or(plugin.active);
                        let name = plugin.name.clone();
                        let on_override = props.on_override.clone();
                        let onchange = Callback::from(move |event: Event| {
                            let input = event.target_unchecked_into::<web_sys::HtmlInputElement>();
                            on_override.emit(PluginOverride { name: name.clone(), enabled: input.checked() });
                        });
                        let policy = match plugin.policy { PluginPolicy::Ask => "Ask", PluginPolicy::Always => "Always", PluginPolicy::Never => "Never" };
                        html! {
                            <label class="plugin-suggestion-pill" title={title}>
                                <input type="checkbox" {checked} {onchange}/>
                                { format!("{} · {}{}", plugin.display_name, policy, if plugin.disabled_by_policy { " · disabled by policy" } else { "" }) }
                            </label>
                        }
                    }) }
                </span>
            }
        </div>
    }
}

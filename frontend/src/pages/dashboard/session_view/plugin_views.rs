//! Controlled plugin views. SessionView owns inventory fetching, surface state,
//! and persisted notice expansion; these components only render and emit intent.
use crate::components::plugin_discovery::{plugin_context_label, suggested_plugins};
use shared::api::PortalPluginInfo;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct PluginContextNoticeProps {
    pub plugins: Vec<PortalPluginInfo>,
    pub expanded: bool,
    pub on_toggle: Callback<MouseEvent>,
}

#[derive(Properties, PartialEq)]
pub(super) struct PluginDiscoverySurfaceProps {
    pub plugins: Vec<PortalPluginInfo>,
    pub error: Option<String>,
    pub collapsed: bool,
    pub fullscreen: bool,
    pub on_toggle_collapsed: Callback<MouseEvent>,
    pub on_toggle_mode: Callback<MouseEvent>,
    pub on_close: Callback<MouseEvent>,
}

#[derive(Properties, PartialEq)]
struct PluginCardProps {
    pub plugin: PortalPluginInfo,
}

#[function_component(PluginContextNotice)]
pub(super) fn plugin_context_notice(props: &PluginContextNoticeProps) -> Html {
    let suggested = suggested_plugins(&props.plugins);
    if suggested.is_empty() {
        return html! {};
    }
    let context_bytes = suggested
        .iter()
        .map(|plugin| plugin.context_bytes)
        .sum::<u64>();
    let estimated_tokens = suggested
        .iter()
        .map(|plugin| plugin.estimated_tokens)
        .sum::<u64>();
    let label = suggested
        .iter()
        .map(|plugin| plugin.display_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let expanded = props.expanded;
    let toggle = props.on_toggle.clone();

    html! {
        <section class={classes!("plugin-context-card", expanded.then_some("expanded"))}>
            <button
                type="button"
                class="plugin-context-summary"
                onclick={toggle}
                aria-expanded={expanded.to_string()}
            >
                <span class="plugin-context-caret">{ if expanded { "▾" } else { "▸" } }</span>
                <span class="plugin-context-title">{ "Plugin context" }</span>
                <span class="plugin-context-names">{ label }</span>
                <span class="plugin-context-size">
                    { plugin_context_label(context_bytes, estimated_tokens) }
                </span>
            </button>
            if expanded {
                <div class="plugin-context-body">
                    { for suggested.into_iter().map(|plugin| {
                        html! {
                            <article class="plugin-context-plugin" key={plugin.name.clone()}>
                                <div class="plugin-context-plugin-title">
                                    <span>{ &plugin.display_name }</span>
                                    <span>{ plugin_context_label(plugin.context_bytes, plugin.estimated_tokens) }</span>
                                </div>
                                if let Some(reason) = plugin.reason.as_deref() {
                                    <div class="plugin-context-detail">{ reason }</div>
                                }
                                if let Some(source) = plugin.source.as_deref() {
                                    <code class="plugin-context-source">{ source }</code>
                                }
                            </article>
                        }
                    }) }
                </div>
            }
        </section>
    }
}

#[function_component(PluginDiscoverySurface)]
pub(super) fn plugin_discovery_surface(props: &PluginDiscoverySurfaceProps) -> Html {
    let suggested_count = suggested_plugins(&props.plugins).len();
    let collapsed = props.collapsed;
    let fullscreen = props.fullscreen;
    let title = if suggested_count > 0 {
        format!(
            "Plugins · {suggested_count} suggested · {} installed",
            props.plugins.len()
        )
    } else {
        format!("Plugins · {} installed", props.plugins.len())
    };
    let status_class = (suggested_count > 0).then_some("is-up");
    let status_title = if suggested_count > 0 {
        "Plugins match this session directory"
    } else {
        "No plugin matched this session directory"
    };
    let toggle_collapsed = props.on_toggle_collapsed.clone();
    let toggle_mode = props.on_toggle_mode.clone();
    let close = props.on_close.clone();

    html! {
        <aside
            id="session-plugins"
            class={classes!(
                "session-forward-surface",
                "session-plugins-surface",
                collapsed.then_some("collapsed"),
                fullscreen.then_some("fullscreen"),
            )}
            aria-label="Plugins"
        >
            <div class="session-forward-toolbar session-plugins-toolbar">
                <button
                    type="button"
                    class="surface-icon-button"
                    title={ if collapsed { "Expand plugins" } else { "Collapse plugins" } }
                    onclick={toggle_collapsed}
                >
                    { if collapsed { "▸" } else { "▾" } }
                </button>
                <span class={classes!("surface-status", status_class)} title={status_title}></span>
                <span class="session-forward-title" title={title.clone()}>{ title }</span>
                <button
                    type="button"
                    class="surface-icon-button surface-mode-button"
                    title={ if fullscreen { "Return to split view" } else { "Full screen" } }
                    onclick={toggle_mode}
                >
                    { if fullscreen { "⇲" } else { "⛶" } }
                </button>
                <button
                    type="button"
                    class="surface-icon-button surface-close-button"
                    title="Close plugins"
                    onclick={close}
                >
                    { "×" }
                </button>
            </div>
            if !collapsed {
                <section class="plugin-panel">
                    <div class="plugin-panel-header">
                        <div class="edit-stack-title">
                            <span class="edit-stack-kicker">{ "Plugin discovery" }</span>
                            <span class="edit-stack-count">
                                { format!("{suggested_count} suggested · {} installed", props.plugins.len()) }
                            </span>
                        </div>
                    </div>
                    if let Some(error) = props.error.as_deref() {
                        <div class="edit-stack-error">{ error }</div>
                    }
                    <div class="plugin-list">
                        if props.plugins.is_empty() {
                            <div class="edit-stack-empty">{ "No installed Portal plugins were discovered on this host." }</div>
                        }
                        { for props.plugins.iter().map(|plugin| html! { <PluginCard key={plugin.name.clone()} plugin={plugin.clone()} /> }) }
                    </div>
                </section>
            }
        </aside>
    }
}

#[function_component(PluginCard)]
fn plugin_card(props: &PluginCardProps) -> Html {
    let plugin = &props.plugin;
    let is_suggested = plugin.active || plugin.suggested;
    html! {
        <article class={classes!("plugin-card", is_suggested.then_some("suggested"))} key={plugin.name.clone()}>
            <div class="plugin-card-topline">
                <div class="plugin-card-title">
                    <span>{ &plugin.display_name }</span>
                    if is_suggested {
                        <span class="plugin-card-badge">{ "suggested" }</span>
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
            if let Some(source) = plugin.source.as_deref() {
                <code class="plugin-card-source">{ source }</code>
            }
            if !plugin.skills.is_empty() {
                <div class="plugin-card-section">
                    <span class="plugin-card-section-title">{ "Skills" }</span>
                    <div class="plugin-skill-list">
                        { for plugin.skills.iter().map(|skill| {
                            let title = skill
                                .description
                                .clone()
                                .unwrap_or_else(|| skill.path.clone());
                            html! {
                                <span class="plugin-skill-pill" title={title} key={skill.name.clone()}>
                                    { &skill.name }
                                </span>
                            }
                        }) }
                    </div>
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
        </article>
    }
}

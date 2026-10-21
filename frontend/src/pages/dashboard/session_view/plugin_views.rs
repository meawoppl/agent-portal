//! Controlled plugin views. SessionView owns inventory fetching, surface state,
//! and persisted notice expansion; these components only render and emit intent.
use crate::components::plugin_discovery::{
    run_plugin_action, suggested_plugins, PluginCard, PLUGINS_BROWSER,
};
use shared::api::{
    PluginAction, PluginActivation, PluginContextEntry, PluginContextKind, PluginPolicy,
    PluginRequest, PortalPluginInfo,
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct PluginContextNoticeProps {
    /// The launcher's record of what this session's process was given.
    pub entries: Vec<PluginContextEntry>,
    /// Current inventory, for display names and project policy.
    pub plugins: Vec<PortalPluginInfo>,
    pub expanded: bool,
    pub on_toggle: Callback<MouseEvent>,
    /// Set when the viewer may change this computer's project policy.
    pub launcher_id: Option<Uuid>,
    pub working_directory: String,
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

fn source_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

fn activation_label(activation: PluginActivation) -> &'static str {
    match activation {
        PluginActivation::Explicit => "chosen for this session",
        PluginActivation::Policy => "project policy",
        PluginActivation::Detected => "detected",
    }
}

/// The plugin context this session is using, from the launcher's record (set at
/// launch, replaced when a selection change reaches the running session),
/// with each file's text one click away rather than in the transcript.
#[function_component(PluginContextNotice)]
pub(super) fn plugin_context_notice(props: &PluginContextNoticeProps) -> Html {
    let disabled = use_state(BTreeSet::<String>::new);
    let error = use_state(|| None::<String>);
    if props.entries.is_empty() {
        return html! {};
    }
    let mut by_plugin: BTreeMap<&str, Vec<&PluginContextEntry>> = BTreeMap::new();
    for entry in &props.entries {
        by_plugin
            .entry(entry.plugin.as_str())
            .or_default()
            .push(entry);
    }
    let display_name = |name: &str| {
        props
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .map_or_else(|| name.to_string(), |plugin| plugin.display_name.clone())
    };
    let estimated_tokens = props
        .entries
        .iter()
        .map(|e| e.estimated_tokens)
        .sum::<u64>();
    let label = by_plugin
        .keys()
        .map(|name| display_name(name))
        .collect::<Vec<_>>()
        .join(", ");
    let expanded = props.expanded;

    let disable = |name: &str| -> Option<Callback<MouseEvent>> {
        let launcher_id = props.launcher_id?;
        let never = props
            .plugins
            .iter()
            .any(|plugin| plugin.name == name && plugin.policy == PluginPolicy::Never);
        if never || disabled.contains(name) {
            return None;
        }
        let name = name.to_string();
        let directory = props.working_directory.clone();
        let (disabled, error) = (disabled.clone(), error.clone());
        Some(Callback::from(move |_: MouseEvent| {
            let request = PluginRequest {
                working_directory: Some(directory.clone()),
                session_id: None,
                action: PluginAction::SetPolicy {
                    name: name.clone(),
                    policy: PluginPolicy::Never,
                },
            };
            let (name, disabled, error) = (name.clone(), disabled.clone(), error.clone());
            spawn_local(async move {
                match run_plugin_action(launcher_id, &request).await {
                    Ok(_) => {
                        let mut names = (*disabled).clone();
                        names.insert(name);
                        disabled.set(names);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(format!("Could not change the project policy: {e}"))),
                }
            });
        }))
    };

    html! {
        <section class={classes!("plugin-context-card", expanded.then_some("expanded"))}>
            <button
                type="button"
                class="plugin-context-summary"
                onclick={props.on_toggle.clone()}
                aria-expanded={expanded.to_string()}
            >
                <span class="plugin-context-caret">{ if expanded { "▾" } else { "▸" } }</span>
                <span class="plugin-context-title">{ "Plugins available to this session" }</span>
                <span class="plugin-context-names">{ label }</span>
                <span class="plugin-context-size">
                    { format!("~{estimated_tokens} estimated catalog tokens") }
                </span>
            </button>
            if expanded {
                <div class="plugin-context-body">
                    if let Some(error) = &*error {
                        <div class="edit-stack-error">{ error }</div>
                    }
                    { for by_plugin.iter().map(|(name, entries)| {
                        let tokens = entries.iter().map(|e| e.estimated_tokens).sum::<u64>();
                        let first = entries[0];
                        html! {
                            <article class="plugin-context-plugin" key={name.to_string()}>
                                <div class="plugin-context-plugin-title">
                                    <span>{ display_name(name) }</span>
                                    <span>{ format!("~{tokens} estimated catalog tokens") }</span>
                                </div>
                                <div class="plugin-context-detail">
                                    { format!("{} · {}", first.reason, activation_label(first.activation)) }
                                </div>
                                { for entries.iter().map(|entry| html! {
                                    <details class="plugin-context-entry" key={entry.path.clone()}>
                                        <summary>
                                            <span class="plugin-context-kind">
                                                { match entry.kind { PluginContextKind::Skill => "skill", PluginContextKind::Prompt => "prompt" } }
                                            </span>
                                            <span>{ &entry.name }</span>
                                            <span class="plugin-context-size">
                                                { format!("{} source · ~{} estimated catalog tokens", source_size(entry.context_bytes), entry.estimated_tokens) }
                                            </span>
                                        </summary>
                                        <code class="plugin-context-source">{ &entry.path }</code>
                                        <pre class="plugin-context-text">{ &entry.text }</pre>
                                    </details>
                                }) }
                                if disabled.contains(*name) {
                                    <div class="plugin-context-detail">{ "Won't load in new sessions in this project." }</div>
                                } else if let Some(onclick) = disable(name) {
                                    <button type="button" class="plugin-context-disable" {onclick}>
                                        { "Disable for this project" }
                                    </button>
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
                        <a class="plugin-browse-link" href={PLUGINS_BROWSER} target="_blank" rel="noopener noreferrer">{ "Browse plugins" }</a>
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

//! Controlled plugin views. SessionView owns inventory fetching, surface state,
//! and persisted notice expansion; these components only render and emit intent.
use crate::components::plugin_discovery::{plugin_context_label, suggested_plugins};
use crate::components::plugin_manager::PluginManager;
use crate::utils;
use shared::api::{PluginContextEntry, PortalPluginInfo};
use uuid::Uuid;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct PluginContextNoticeProps {
    pub entries: Vec<PluginContextEntry>,
    pub expanded: bool,
    pub feedback: Option<String>,
    pub can_disable: bool,
    pub on_toggle: Callback<MouseEvent>,
    pub on_disable: Callback<String>,
}

#[derive(Properties, PartialEq)]
pub(super) struct PluginDiscoverySurfaceProps {
    pub plugins: Vec<PortalPluginInfo>,
    pub error: Option<String>,
    pub collapsed: bool,
    pub fullscreen: bool,
    pub session_id: Uuid,
    pub launcher_id: Option<Uuid>,
    pub working_directory: String,
    pub tab_ready: bool,
    pub pending_port: Option<(u16, bool)>,
    pub current_forward_port: Option<u16>,
    pub on_toggle_collapsed: Callback<MouseEvent>,
    pub on_toggle_mode: Callback<MouseEvent>,
    pub on_close: Callback<MouseEvent>,
    pub on_cancel_port: Callback<MouseEvent>,
    pub on_confirm_port: Callback<(u16, bool)>,
    pub on_request_port: Callback<(u16, bool)>,
}

#[function_component(PluginContextNotice)]
pub(super) fn plugin_context_notice(props: &PluginContextNoticeProps) -> Html {
    // A later inventory scan cannot establish what the agent actually saw.
    // Only the launch-time snapshot belongs in transcript provenance.
    let entries = &props.entries;
    if entries.is_empty() {
        return html! {};
    }
    let bytes = entries.iter().map(|entry| entry.context_bytes).sum();
    let tokens = entries.iter().map(|entry| entry.estimated_tokens).sum();
    let expanded = props.expanded;
    html! {
        <section class={classes!("plugin-context-card", expanded.then_some("expanded"))}>
            <button type="button" class="plugin-context-summary"
                onclick={props.on_toggle.clone()}
                aria-expanded={expanded.to_string()}>
                <span>{ if expanded { "▾" } else { "▸" } }</span>
                <span class="plugin-context-title">{ "Injected plugin context" }</span>
                <span>{ format!("{} entries · {}", entries.len(), plugin_context_label(bytes, tokens)) }</span>
            </button>
            if expanded {
                <div class="plugin-context-body">
                    if let Some(feedback) = &props.feedback { <p role="status">{feedback}</p> }
                    { for entries.iter().map(|entry| {
                        let name = entry.plugin.clone();
                        html! {
                            <article class="plugin-context-plugin" key={format!("{}:{}:{}", entry.plugin, entry.name, entry.path)}>
                                <div class="plugin-context-plugin-title">{format!("{} / {}", entry.plugin, entry.name)}</div>
                                <div>{format!("{:?} · {:?} · {}", entry.kind, entry.activation, plugin_context_label(entry.context_bytes, entry.estimated_tokens))}</div>
                                <p>{&entry.reason}</p>
                                <code class="plugin-context-source">{&entry.path}</code>
                                <details><summary>{"Full injected text"}</summary><pre>{&entry.text}</pre></details>
                                if props.can_disable {
                                    <button type="button" onclick={props.on_disable.reform(move |_| name.clone())}>{"Disable for future project sessions"}</button>
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
                    <a href="/plugins">{"Browse all plugins"}</a>
                    if props.tab_ready {
                        <p role="status"><a href={utils::api_url(&format!("/api/sessions/{}/forwards/open", props.session_id))} target="_blank" rel="noopener noreferrer">{"Surface ready — open in new tab"}</a></p>
                    }
                    if let Some((port, new_tab)) = props.pending_port {
                        <crate::components::ConfirmModal
                            title="Replace session forward?"
                            message={format!("This session currently forwards port {}. Opening this plugin will replace it with port {port}, disconnecting access to the previous service.", props.current_forward_port.unwrap_or_default())}
                            on_cancel={props.on_cancel_port.clone()}
                            on_confirm={props.on_confirm_port.reform(move |_| (port, new_tab))} />
                    }
                    if let Some(error) = &props.error { <p role="alert">{error}</p> }
                    if let Some(launcher_id) = props.launcher_id {
                        <PluginManager key={format!("{}:{}:{}", launcher_id, props.session_id, props.working_directory)}
                            {launcher_id} session_id={Some(props.session_id)}
                            working_directory={props.working_directory.clone()}
                            on_open_surface={props.on_request_port.reform(|port| (port, false))}
                            on_open_surface_tab={props.on_request_port.reform(|port| (port, true))} />
                    } else {
                        <p>{"Plugin controls require a session attached to a launcher."}</p>
                    }
                </section>
            }
        </aside>
    }
}

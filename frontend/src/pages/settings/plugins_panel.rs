//! Settings ▸ Plugins: the plugins installed on each computer, optionally
//! matched against a project directory.

use shared::api::PluginInventoryResponse;
use shared::LauncherInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::components::plugin_discovery::{fetch_launcher_plugins, PluginCard};
use crate::utils;

const PLUGINS_REPO: &str = "https://github.com/meawoppl/agent-portal-plugins";

fn manages_plugins(launcher: &LauncherInfo) -> bool {
    launcher.connected
        && launcher
            .capabilities
            .iter()
            .any(|c| c == shared::LAUNCHER_CAPABILITY_PLUGINS)
}

#[function_component(PluginsPanel)]
pub fn plugins_panel() -> Html {
    let launchers = use_state(|| None::<Result<Vec<LauncherInfo>, String>>);
    let selected = use_state(|| None::<Uuid>);
    let directory_draft = use_state(String::new);
    let directory = use_state(|| None::<String>);
    let inventory = use_state(|| None::<Result<PluginInventoryResponse, String>>);

    {
        let launchers = launchers.clone();
        let selected = selected.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match utils::fetch_launchers().await {
                    Ok(list) => {
                        selected.set(
                            list.iter()
                                .find(|l| manages_plugins(l))
                                .or_else(|| list.first())
                                .map(|l| l.launcher_id),
                        );
                        launchers.set(Some(Ok(list)));
                    }
                    Err(e) => launchers.set(Some(Err(e.to_string()))),
                }
            });
            || ()
        });
    }

    let selected_launcher = match (&*launchers, *selected) {
        (Some(Ok(list)), Some(id)) => list.iter().find(|l| l.launcher_id == id).cloned(),
        _ => None,
    };
    let can_manage = selected_launcher.as_ref().is_some_and(manages_plugins);

    {
        let inventory = inventory.clone();
        use_effect_with(
            (*selected, (*directory).clone(), can_manage),
            move |(launcher, directory, can_manage)| {
                inventory.set(None);
                if let (Some(launcher), true) = (*launcher, *can_manage) {
                    let (launcher, directory) = (launcher, directory.clone());
                    spawn_local(async move {
                        inventory.set(Some(fetch_launcher_plugins(launcher, directory).await));
                    });
                }
                || ()
            },
        );
    }

    let on_select = {
        let selected = selected.clone();
        Callback::from(move |e: Event| {
            let value = e
                .target_unchecked_into::<web_sys::HtmlSelectElement>()
                .value();
            selected.set(Uuid::parse_str(&value).ok());
        })
    };
    let on_directory_input = {
        let directory_draft = directory_draft.clone();
        Callback::from(move |e: InputEvent| {
            directory_draft.set(e.target_unchecked_into::<HtmlInputElement>().value());
        })
    };
    let on_match = {
        let directory_draft = directory_draft.clone();
        let directory = directory.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let draft = directory_draft.trim().to_string();
            directory.set((!draft.is_empty()).then_some(draft));
        })
    };

    let picker = match &*launchers {
        None => html! { <p class="schedule-day-empty">{ "Loading computers…" }</p> },
        Some(Err(error)) => {
            html! { <p class="settings-error">{ format!("Could not load computers: {error}") }</p> }
        }
        Some(Ok(list)) if list.is_empty() => html! {
            <p class="schedule-day-empty">{ "No computers yet. Connect one from the Computers tab." }</p>
        },
        Some(Ok(list)) => html! {
            <form class="plugins-browser-controls" onsubmit={on_match}>
                <label>
                    { "Computer" }
                    <select onchange={on_select}>
                        { for list.iter().map(|l| html! {
                            <option
                                value={l.launcher_id.to_string()}
                                selected={Some(l.launcher_id) == *selected}
                            >
                                { format!("{}{}", l.launcher_name, if l.connected { "" } else { " (offline)" }) }
                            </option>
                        }) }
                    </select>
                </label>
                <label class="plugins-browser-directory">
                    { "Match a project directory" }
                    <input
                        type="text"
                        placeholder="~/repos/my-board"
                        value={(*directory_draft).clone()}
                        oninput={on_directory_input}
                    />
                </label>
                <button type="submit" class="sched-btn">{ "Match" }</button>
            </form>
        },
    };

    let body = match (&selected_launcher, &*inventory) {
        (None, _) => html! {},
        (Some(l), _) if !l.connected => html! {
            <p class="schedule-day-empty">{ format!("{} is offline.", l.launcher_name) }</p>
        },
        (Some(l), _) if !can_manage => html! {
            <p class="schedule-day-empty">
                { format!("Update the launcher on {} to browse its plugins.", l.launcher_name) }
            </p>
        },
        (_, None) => html! {
            <div class="loading"><div class="spinner"></div><p>{ "Loading plugins…" }</p></div>
        },
        (_, Some(Err(error))) => {
            html! { <p class="settings-error">{ format!("Could not load plugins: {error}") }</p> }
        }
        (Some(l), Some(Ok(data))) if data.plugins.is_empty() => html! {
            <div class="schedule-day-empty">
                <p>{ format!("No plugins are installed on {}.", l.launcher_name) }</p>
                <code>{ "agent-portal plugin install github:meawoppl/agent-portal-plugins//<name>" }</code>
            </div>
        },
        (_, Some(Ok(data))) => {
            let matched = data.plugins.iter().filter(|p| p.suggested).count();
            html! {
                <>
                    if let Some(dir) = data.working_directory.as_deref() {
                        <p class="section-description">
                            { if data.scanned {
                                format!("{matched} of {} match {dir}; those load into new sessions there.", data.plugins.len())
                            } else {
                                format!("{dir} is not a directory on this computer.")
                            } }
                        </p>
                    }
                    { for data.warnings.iter().map(|w| html! { <p class="schedule-warning">{ w }</p> }) }
                    <div class="plugins-browser-grid">
                        { for data.plugins.iter().map(|plugin| html! {
                            <PluginCard key={plugin.name.clone()} plugin={plugin.clone()} />
                        }) }
                    </div>
                </>
            }
        }
    };

    html! {
        <section class="section-stack plugins-browser">
            <div class="section-header">
                <h2>{ "Plugins" }</h2>
                <p class="section-description">
                    { "Plugins add domain tools, skills and commands to sessions on a computer. Browse more in " }
                    <a href={PLUGINS_REPO} target="_blank" rel="noopener noreferrer">{ "agent-portal-plugins" }</a>
                    { "." }
                </p>
            </div>
            { picker }
            { body }
        </section>
    }
}

//! Settings ▸ Plugins: the plugins installed on each computer, optionally
//! matched against a project directory.

use std::cell::Cell;
use std::rc::Rc;

use shared::api::{
    PluginAction, PluginInventoryResponse, PluginRequest, PluginResponse, PortalPluginInfo,
};
use shared::LauncherInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::components::plugin_discovery::{fetch_launcher_plugins, run_plugin_action, PluginCard};
use crate::utils;

const PLUGINS_REPO: &str = "https://github.com/meawoppl/agent-portal-plugins";

/// A destructive action waiting for the user to confirm it.
#[derive(Clone, PartialEq)]
struct Pending {
    label: String,
    warning: String,
    action: PluginAction,
}

/// The last finished action, kept on screen with its output.
#[derive(Clone, PartialEq)]
struct Outcome {
    label: String,
    result: Result<PluginResponse, String>,
}

fn render_outcome(outcome: &Outcome) -> Html {
    let (ok, summary, output) = match &outcome.result {
        Ok(response) => {
            let status = match (response.success, response.exit_code) {
                (true, _) => "done".to_string(),
                (false, Some(code)) => format!("failed (exit {code})"),
                (false, None) => "failed".to_string(),
            };
            (
                response.success,
                format!("{}: {status}", outcome.label),
                Some((response.error.clone(), response.output.clone())),
            )
        }
        Err(error) => (false, format!("{}: {error}", outcome.label), None),
    };
    html! {
        <div class={classes!("plugin-outcome", (!ok).then_some("failed"))} role="status">
            <strong>{ summary }</strong>
            if let Some((error, output)) = output {
                if let Some(error) = error {
                    <p>{ error }</p>
                }
                if !output.trim().is_empty() {
                    <details open={!ok}>
                        <summary>{ "Output" }</summary>
                        <pre>{ output }</pre>
                    </details>
                }
            }
        </div>
    }
}

/// Lifecycle buttons for one installed plugin.
fn plugin_actions(
    plugin: &PortalPluginInfo,
    host: &str,
    disabled: bool,
    run: &Callback<(String, PluginAction)>,
    confirm: &Callback<Pending>,
) -> Html {
    let name = plugin.name.clone();
    let title = plugin.display_name.clone();
    let update = {
        let run = run.clone();
        let (name, title) = (name.clone(), title.clone());
        Callback::from(move |_| {
            run.emit((
                format!("Update {title}"),
                PluginAction::Update { name: name.clone() },
            ))
        })
    };
    let doctor = {
        let run = run.clone();
        let (name, title) = (name.clone(), title.clone());
        Callback::from(move |_| {
            run.emit((
                format!("Doctor {title}"),
                PluginAction::Doctor { name: name.clone() },
            ))
        })
    };
    let reinstall = plugin.source.clone().map(|source| {
        let confirm = confirm.clone();
        let title = title.clone();
        let host = host.to_string();
        Callback::from(move |_| {
            confirm.emit(Pending {
                label: format!("Reinstall {title}"),
                warning: format!(
                    "Reinstall {title} on {host} from {source}? Its checkout is replaced and setup runs again."
                ),
                action: PluginAction::Install {
                    source: source.clone(),
                    force: true,
                },
            })
        })
    });
    let uninstall = {
        let confirm = confirm.clone();
        let host = host.to_string();
        Callback::from(move |_| {
            confirm.emit(Pending {
                label: format!("Uninstall {title}"),
                warning: format!(
                    "Uninstall {title} from {host}? Its checkout and plugin data are deleted."
                ),
                action: PluginAction::Remove { name: name.clone() },
            })
        })
    };
    html! {
        <div class="plugin-card-actions">
            <button
                class={classes!("sched-btn", (plugin.update_available == Some(true)).then_some("sched-btn-primary"))}
                {disabled}
                onclick={update}
            >{ "Update" }</button>
            if plugin.has_doctor {
                <button class="sched-btn" {disabled} onclick={doctor}>{ "Doctor" }</button>
            }
            if let Some(reinstall) = reinstall {
                <button class="sched-btn" {disabled} onclick={reinstall}>{ "Reinstall" }</button>
            }
            <button class="sched-btn sched-btn-danger" {disabled} onclick={uninstall}>{ "Uninstall" }</button>
        </div>
    }
}

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
    let busy = use_state(|| None::<String>);
    let outcome = use_state(|| None::<Outcome>);
    let pending = use_state(|| None::<Pending>);
    let install_draft = use_state(String::new);

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
                // A slow answer for the previous computer or directory must not
                // land under the one now selected.
                let current = Rc::new(Cell::new(true));
                if let (Some(launcher), true) = (*launcher, *can_manage) {
                    let (launcher, directory) = (launcher, directory.clone());
                    let current = current.clone();
                    spawn_local(async move {
                        let result = fetch_launcher_plugins(launcher, directory).await;
                        if current.get() {
                            inventory.set(Some(result));
                        }
                    });
                }
                move || current.set(false)
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

    // Actions run on the selected computer and answer with its refreshed
    // inventory, which replaces the one on screen.
    let run = {
        let selected = *selected;
        let directory = (*directory).clone();
        let (inventory, busy, outcome) = (inventory.clone(), busy.clone(), outcome.clone());
        Callback::from(move |(label, action): (String, PluginAction)| {
            let Some(launcher) = selected else { return };
            let request = PluginRequest {
                working_directory: directory.clone(),
                session_id: None,
                action,
            };
            let (inventory, busy, outcome) = (inventory.clone(), busy.clone(), outcome.clone());
            busy.set(Some(label.clone()));
            outcome.set(None);
            spawn_local(async move {
                let result = run_plugin_action(launcher, &request).await;
                if let Some(updated) = result.as_ref().ok().and_then(|r| r.inventory.clone()) {
                    inventory.set(Some(Ok(updated)));
                }
                busy.set(None);
                outcome.set(Some(Outcome { label, result }));
            });
        })
    };
    let confirm = {
        let pending = pending.clone();
        Callback::from(move |action: Pending| pending.set(Some(action)))
    };
    let on_install_input = {
        let install_draft = install_draft.clone();
        Callback::from(move |e: InputEvent| {
            install_draft.set(e.target_unchecked_into::<HtmlInputElement>().value());
        })
    };
    let on_install = {
        let install_draft = install_draft.clone();
        let run = run.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let source = install_draft.trim().to_string();
            if source.is_empty() {
                return;
            }
            install_draft.set(String::new());
            run.emit((
                format!("Install {source}"),
                PluginAction::Install {
                    source,
                    force: false,
                },
            ));
        })
    };
    let on_check_updates = {
        let run = run.clone();
        Callback::from(move |_| run.emit(("Check for updates".into(), PluginAction::CheckUpdates)))
    };
    let is_busy = busy.is_some();
    let host = selected_launcher
        .as_ref()
        .map(|l| l.launcher_name.clone())
        .unwrap_or_default();

    let manage = if can_manage {
        html! {
            <>
                <form class="plugins-browser-controls" onsubmit={on_install}>
                    <label class="plugins-browser-directory">
                        { "Install from a source" }
                        <input
                            type="text"
                            placeholder="github:meawoppl/agent-portal-plugins//kicadmium"
                            value={(*install_draft).clone()}
                            oninput={on_install_input}
                            disabled={is_busy}
                        />
                    </label>
                    <button type="submit" class="sched-btn sched-btn-primary" disabled={is_busy}>{ "Install" }</button>
                    <button type="button" class="sched-btn" disabled={is_busy} onclick={on_check_updates}>
                        { "Check for updates" }
                    </button>
                </form>
                if let Some(label) = &*busy {
                    <div class="plugin-outcome running" role="status">
                        <div class="spinner"></div>
                        <strong>{ format!("{label} on {host}…") }</strong>
                    </div>
                }
                if let Some(action) = &*pending {
                    <div class="plugin-confirm" role="alertdialog">
                        <p>{ &action.warning }</p>
                        <button class="sched-btn sched-btn-danger" onclick={{
                            let (run, pending, action) = (run.clone(), pending.clone(), action.clone());
                            Callback::from(move |_| {
                                pending.set(None);
                                run.emit((action.label.clone(), action.action.clone()));
                            })
                        }}>{ &action.label }</button>
                        <button class="sched-btn" onclick={{
                            let pending = pending.clone();
                            Callback::from(move |_| pending.set(None))
                        }}>{ "Cancel" }</button>
                    </div>
                }
                if let Some(outcome) = &*outcome {
                    { render_outcome(outcome) }
                }
            </>
        }
    } else {
        html! {}
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
            <p class="schedule-day-empty">
                { format!("No plugins are installed on {}. Install one above.", l.launcher_name) }
            </p>
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
                            <PluginCard key={plugin.name.clone()} plugin={plugin.clone()}>
                                { plugin_actions(plugin, &host, is_busy, &run, &confirm) }
                            </PluginCard>
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
            { manage }
            { body }
        </section>
    }
}

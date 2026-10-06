use crate::components::plugin_manager::PluginManager;
use crate::utils;
use shared::LauncherInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[function_component(PluginsPage)]
pub fn plugins_page() -> Html {
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let selected = use_state(|| None::<Uuid>);
    let directory = use_state(String::new);
    let project = use_state(String::new);
    let error = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let refresh = use_state(|| 0u32);
    {
        let launchers = launchers.clone();
        let selected = selected.clone();
        let directory = directory.clone();
        let project = project.clone();
        let error = error.clone();
        let loading = loading.clone();
        use_effect_with(*refresh, move |_| {
            loading.set(true);
            spawn_local(async move {
                match utils::fetch_launchers().await {
                    Ok(hosts) => {
                        if selected.is_none() {
                            if let Some(host) = hosts.iter().find(|host| host.connected) {
                                selected.set(Some(host.launcher_id));
                                let cwd = host.working_directory.clone().unwrap_or_default();
                                directory.set(cwd.clone());
                                project.set(cwd);
                            }
                        }
                        launchers.set(hosts);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.to_string())),
                }
                loading.set(false);
            });
            || ()
        });
    }
    let select = {
        let selected = selected.clone();
        let launchers = launchers.clone();
        let directory = directory.clone();
        let project = project.clone();
        Callback::from(move |e: Event| {
            let id = e
                .target_unchecked_into::<HtmlSelectElement>()
                .value()
                .parse::<Uuid>()
                .ok();
            selected.set(id);
            let cwd = launchers
                .iter()
                .find(|host| Some(host.launcher_id) == id)
                .and_then(|host| host.working_directory.clone())
                .unwrap_or_default();
            directory.set(cwd.clone());
            project.set(cwd);
        })
    };
    html! {<main class="plugin-manager">
        <style>{include_str!("../../styles/plugins.css")}</style>
        <a href="/dashboard">{"← Dashboard"}</a><h1>{"Plugins"}</h1>
        <p>{"Browse and manage plugins installed on your launcher hosts. Project policy controls which plugins activate for new sessions."}</p>
        <div class="plugin-toolbar"><label>{"Host"}<select value={selected.map(|id| id.to_string()).unwrap_or_default()} onchange={select}><option value="">{"Select host"}</option>{for launchers.iter().map(|host| html!{<option value={host.launcher_id.to_string()} disabled={!host.connected}>{format!("{} ({}){}", host.launcher_name, host.hostname, if host.connected {""} else {" — offline"})}</option>})}</select></label>
        <label>{"Project directory"}<input value={(*directory).clone()} oninput={{let directory = directory.clone(); Callback::from(move |e: InputEvent| directory.set(e.target_unchecked_into::<HtmlInputElement>().value()))}} /></label>
        <button onclick={{let project = project.clone(); let directory = directory.clone(); Callback::from(move |_| project.set((*directory).clone()))}}>{"Select project"}</button>
        <button disabled={*loading} onclick={{let refresh = refresh.clone(); Callback::from(move |_| refresh.set(*refresh + 1))}}>{"Refresh hosts"}</button></div>
        if *loading {<p role="status">{"Loading hosts…"}</p>}
        if let Some(error) = &*error {<p role="alert">{error}</p>}
        if !*loading && launchers.is_empty() {<p>{"Connect a launcher to manage its plugins."}</p>}
        if let Some(host) = *selected {<PluginManager key={format!("{host}:{}", *project)} launcher_id={host} working_directory={(*project).clone()} />}
    </main>}
}

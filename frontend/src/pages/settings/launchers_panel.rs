use crate::utils::{self, On401};
use gloo_net::http::Request;
use shared::LauncherInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
struct LauncherRowProps {
    launcher: LauncherInfo,
    on_update: Callback<Uuid>,
    update_in_progress: bool,
}

#[function_component(LauncherRow)]
fn launcher_row(props: &LauncherRowProps) -> Html {
    let l = &props.launcher;
    let on_update = props.on_update.clone();
    let launcher_id = l.launcher_id;

    let (update_label, update_class) = if props.update_in_progress {
        ("Restarting...", "update-button stage-3")
    } else {
        ("Update & Restart", "update-button stage-0")
    };

    let on_update_click = {
        let on_update = on_update.clone();
        Callback::from(move |_| {
            let confirmed = web_sys::window()
                .and_then(|window| {
                    window
                        .confirm_with_message(
                            "Update this launcher to the latest release and restart it?",
                        )
                        .ok()
                })
                .unwrap_or(false);
            if confirmed {
                on_update.emit(launcher_id);
            }
        })
    };

    html! {
        <tr class="token-row">
            <td class="token-name">{ &l.launcher_name }</td>
            <td>{ &l.hostname }</td>
            <td>{ format!("v{}", &l.version) }</td>
            <td>{ l.running_sessions }</td>
            <td class="token-actions">
                <button
                    class={update_class}
                    onclick={on_update_click}
                    disabled={props.update_in_progress}
                    title="Pull the latest agent-portal release and restart this launcher"
                >
                    { update_label }
                </button>
            </td>
        </tr>
    }
}

#[function_component(LaunchersPanel)]
pub fn launchers_panel() -> Html {
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let loading = use_state(|| true);
    let action_result = use_state(|| None::<(bool, String)>);
    let update_in_progress = use_state(|| None::<Uuid>);

    let fetch_launchers = {
        let launchers = launchers.clone();
        let loading = loading.clone();

        Callback::from(move |_| {
            let launchers = launchers.clone();
            let loading = loading.clone();

            spawn_local(async move {
                if let Ok(data) =
                    utils::fetch_json::<Vec<LauncherInfo>>("/api/launchers", On401::Ignore).await
                {
                    launchers.set(data);
                }
                loading.set(false);
            });
        })
    };

    {
        let fetch = fetch_launchers.clone();
        use_effect_with((), move |_| {
            fetch.emit(());
            || ()
        });
    }

    let on_update = {
        let action_result = action_result.clone();
        let update_in_progress = update_in_progress.clone();
        Callback::from(move |launcher_id: Uuid| {
            let action_result = action_result.clone();
            let update_in_progress = update_in_progress.clone();
            spawn_local(async move {
                update_in_progress.set(Some(launcher_id));
                let url = utils::api_url(&format!("/api/launchers/{}/update", launcher_id));
                match Request::post(&url).send().await {
                    Ok(resp) => {
                        if resp.status() == 200 {
                            action_result.set(Some((
                                true,
                                "Update requested. The launcher will fetch the latest release and restart.".to_string(),
                            )));
                        } else {
                            let text = resp.text().await.unwrap_or_default();
                            action_result.set(Some((
                                false,
                                format!("Update failed: {} {}", resp.status(), text),
                            )));
                        }
                    }
                    Err(e) => {
                        action_result.set(Some((false, format!("Update request failed: {:?}", e))));
                    }
                }
                update_in_progress.set(None);
            });
        })
    };

    html! {
        <section class="tokens-section">
            <div class="section-header">
                <h2>{ "Launchers" }</h2>
                <p class="section-description">
                    { "Connected launcher daemons. Authentication tokens do not expire and are \
                       managed automatically." }
                </p>
            </div>

            if let Some((success, message)) = &*action_result {
                <div class={if *success { "token-created-success" } else { "error-message" }}>
                    <p>{ message }</p>
                </div>
            }

            if *loading {
                <div class="loading">
                    <div class="spinner"></div>
                    <p>{ "Loading launchers..." }</p>
                </div>
            } else if launchers.is_empty() {
                <div class="empty-state">
                    <p>{ "No launchers connected. Install agent-portal on a machine to get started." }</p>
                </div>
            } else {
                <div class="table-container">
                    <table class="tokens-table">
                        <thead>
                            <tr>
                                <th>{ "Name" }</th>
                                <th>{ "Host" }</th>
                                <th>{ "Version" }</th>
                                <th>{ "Sessions" }</th>
                                <th>{ "Actions" }</th>
                            </tr>
                        </thead>
                        <tbody>
                            { for launchers.iter().map(|l| {
                                html! {
                                    <LauncherRow
                                        key={l.launcher_id.to_string()}
                                        launcher={l.clone()}
                                        on_update={on_update.clone()}
                                        update_in_progress={*update_in_progress == Some(l.launcher_id)}
                                    />
                                }
                            }) }
                        </tbody>
                    </table>
                </div>
            }
        </section>
    }
}

use crate::utils;
use gloo_net::http::Request;
use shared::LauncherInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

fn days_until_expiration(expires_at: &str) -> Option<i64> {
    let now = js_sys::Date::now();
    let expires = js_sys::Date::parse(expires_at);
    if expires.is_nan() {
        return None;
    }
    let diff_ms = expires - now;
    let diff_days = (diff_ms / (1000.0 * 60.0 * 60.0 * 24.0)).floor() as i64;
    Some(diff_days)
}

pub fn count_expiring_launchers(launchers: &[LauncherInfo]) -> usize {
    launchers
        .iter()
        .filter(|l| {
            l.token_expires_at
                .as_ref()
                .and_then(|exp| days_until_expiration(exp))
                .map(|d| d <= 7)
                .unwrap_or(false)
        })
        .count()
}

#[derive(Properties, PartialEq)]
struct LauncherRowProps {
    launcher: LauncherInfo,
    on_renew: Callback<Uuid>,
    on_update: Callback<Uuid>,
    update_in_progress: bool,
}

#[function_component(LauncherRow)]
fn launcher_row(props: &LauncherRowProps) -> Html {
    let l = &props.launcher;
    let on_renew = props.on_renew.clone();
    let on_update = props.on_update.clone();
    let launcher_id = l.launcher_id;

    let (status_class, status_text) = if let Some(ref exp) = l.token_expires_at {
        let days = days_until_expiration(exp);
        match days {
            Some(d) if d < 0 => ("token-status expired", "Expired".to_string()),
            Some(0) => ("token-status expiring-soon", "Expires today".to_string()),
            Some(1) => ("token-status expiring-soon", "Expires tomorrow".to_string()),
            Some(d) if d <= 7 => (
                "token-status expiring-soon",
                format!("Expires in {} days", d),
            ),
            Some(_) => ("token-status active", "Active".to_string()),
            None => ("token-status active", "Active".to_string()),
        }
    } else {
        ("token-status active", "Dev mode".to_string())
    };

    let needs_renewal = l
        .token_expires_at
        .as_ref()
        .and_then(|exp| days_until_expiration(exp))
        .map(|d| d <= 7)
        .unwrap_or(false);

    let on_renew_click = Callback::from(move |_| {
        on_renew.emit(launcher_id);
    });

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
            <td class="token-expires">
                { l.token_expires_at.as_ref().map(|t| utils::format_timestamp(t)).unwrap_or_else(|| "N/A".to_string()) }
            </td>
            <td class={status_class}>{ status_text }</td>
            <td class="token-actions">
                if needs_renewal {
                    <button class="submit-button" onclick={on_renew_click}>
                        { "Renew Token" }
                    </button>
                }
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

#[derive(Properties, PartialEq)]
pub struct LaunchersPanelProps {
    pub on_launchers_loaded: Callback<Vec<LauncherInfo>>,
}

#[function_component(LaunchersPanel)]
pub fn launchers_panel(props: &LaunchersPanelProps) -> Html {
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let loading = use_state(|| true);
    let renew_result = use_state(|| None::<(bool, String)>);
    let update_in_progress = use_state(|| None::<Uuid>);

    let fetch_launchers = {
        let launchers = launchers.clone();
        let loading = loading.clone();
        let on_loaded = props.on_launchers_loaded.clone();

        Callback::from(move |_| {
            let launchers = launchers.clone();
            let loading = loading.clone();
            let on_loaded = on_loaded.clone();

            spawn_local(async move {
                let url = utils::api_url("/api/launchers");
                if let Ok(resp) = Request::get(&url).send().await {
                    if let Ok(data) = resp.json::<Vec<LauncherInfo>>().await {
                        on_loaded.emit(data.clone());
                        launchers.set(data);
                    }
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

    let on_renew = {
        let renew_result = renew_result.clone();
        let fetch_launchers = fetch_launchers.clone();

        Callback::from(move |launcher_id: Uuid| {
            let renew_result = renew_result.clone();
            let fetch_launchers = fetch_launchers.clone();

            spawn_local(async move {
                let url = utils::api_url(&format!("/api/launchers/{}/renew-token", launcher_id));
                match Request::post(&url).send().await {
                    Ok(resp) => {
                        if resp.status() == 200 {
                            renew_result.set(Some((
                                true,
                                "Token renewed successfully. The launcher will use it on next heartbeat.".to_string(),
                            )));
                            fetch_launchers.emit(());
                        } else {
                            let text = resp.text().await.unwrap_or_default();
                            renew_result.set(Some((
                                false,
                                format!("Failed to renew: {} {}", resp.status(), text),
                            )));
                        }
                    }
                    Err(e) => {
                        renew_result.set(Some((false, format!("Request failed: {:?}", e))));
                    }
                }
            });
        })
    };

    let on_update = {
        let renew_result = renew_result.clone();
        let update_in_progress = update_in_progress.clone();
        Callback::from(move |launcher_id: Uuid| {
            let renew_result = renew_result.clone();
            let update_in_progress = update_in_progress.clone();
            spawn_local(async move {
                update_in_progress.set(Some(launcher_id));
                let url = utils::api_url(&format!("/api/launchers/{}/update", launcher_id));
                match Request::post(&url).send().await {
                    Ok(resp) => {
                        if resp.status() == 200 {
                            renew_result.set(Some((
                                true,
                                "Update requested. The launcher will fetch the latest release and restart.".to_string(),
                            )));
                        } else {
                            let text = resp.text().await.unwrap_or_default();
                            renew_result.set(Some((
                                false,
                                format!("Update failed: {} {}", resp.status(), text),
                            )));
                        }
                    }
                    Err(e) => {
                        renew_result.set(Some((false, format!("Update request failed: {:?}", e))));
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
                    { "Connected launcher daemons and their authentication token status. " }
                    { "Tokens auto-renew when within 7 days of expiry. Use the Renew button for immediate renewal." }
                </p>
            </div>

            if let Some((success, message)) = &*renew_result {
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
                                <th>{ "Token Expires" }</th>
                                <th>{ "Status" }</th>
                                <th>{ "Actions" }</th>
                            </tr>
                        </thead>
                        <tbody>
                            { for launchers.iter().map(|l| {
                                html! {
                                    <LauncherRow
                                        key={l.launcher_id.to_string()}
                                        launcher={l.clone()}
                                        on_renew={on_renew.clone()}
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

mod sessions_panel;
mod sounds_panel;
mod tasks_panel;
mod tokens_panel;

use sessions_panel::SessionsPanel;
use shared::api::ScheduledTaskInfo;
use shared::{ProxyTokenInfo, SessionInfo};
use sounds_panel::SoundsPanel;
use tasks_panel::TasksPanel;
use tokens_panel::{count_expiring_tokens, TokensPanel};
use yew::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum SettingsTab {
    Sessions,
    Tokens,
    Tasks,
    Sounds,
}

#[derive(Properties, PartialEq)]
pub struct SettingsPageProps {
    pub on_close: Callback<()>,
}

#[function_component(SettingsPage)]
pub fn settings_page(props: &SettingsPageProps) -> Html {
    let active_tab = use_state(|| SettingsTab::Sessions);

    // Counts for tab badges (updated when panels load their data)
    let session_count = use_state(|| 0usize);
    let expiring_token_count = use_state(|| 0usize);
    let task_count = use_state(|| 0usize);

    let on_sessions_loaded = {
        let session_count = session_count.clone();
        Callback::from(move |sessions: Vec<SessionInfo>| {
            session_count.set(sessions.len());
        })
    };

    let on_tokens_loaded = {
        let expiring_token_count = expiring_token_count.clone();
        Callback::from(move |tokens: Vec<ProxyTokenInfo>| {
            expiring_token_count.set(count_expiring_tokens(&tokens));
        })
    };

    let on_tasks_loaded = {
        let task_count = task_count.clone();
        Callback::from(move |tasks: Vec<ScheduledTaskInfo>| {
            task_count.set(tasks.len());
        })
    };

    let on_sessions_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(SettingsTab::Sessions))
    };

    let on_tokens_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(SettingsTab::Tokens))
    };

    let on_tasks_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(SettingsTab::Tasks))
    };

    let on_sounds_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(SettingsTab::Sounds))
    };

    let go_back = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };

    html! {
        <div class="settings-container">
            <header class="settings-header">
                <button class="header-button" onclick={go_back}>
                    { "< Back" }
                </button>
                <h1>{ "Settings" }</h1>
                <button class="header-button logout" onclick={Callback::from(|_| {
                    if let Some(window) = web_sys::window() {
                        let _ = window.location().set_href("/api/auth/logout");
                    }
                })}>
                    { "Logout" }
                </button>
            </header>

            <nav class="settings-tabs">
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Sessions).then_some("active"))}
                    onclick={on_sessions_tab}
                >
                    { "Sessions" }
                    <span class="count-badge">{ *session_count }</span>
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Tokens).then_some("active"))}
                    onclick={on_tokens_tab}
                >
                    { "Credentials" }
                    if *expiring_token_count > 0 {
                        <span class="expiring-badge">{ *expiring_token_count }</span>
                    }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Tasks).then_some("active"))}
                    onclick={on_tasks_tab}
                >
                    { "Tasks" }
                    if *task_count > 0 {
                        <span class="count-badge">{ *task_count }</span>
                    }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Sounds).then_some("active"))}
                    onclick={on_sounds_tab}
                >
                    { "Sounds" }
                </button>
            </nav>

            <main class="settings-content">
                if *active_tab == SettingsTab::Tokens {
                    <TokensPanel on_tokens_loaded={on_tokens_loaded} />
                }
                if *active_tab == SettingsTab::Tasks {
                    <TasksPanel on_tasks_loaded={on_tasks_loaded} />
                }
                if *active_tab == SettingsTab::Sounds {
                    <SoundsPanel />
                }
                if *active_tab == SettingsTab::Sessions {
                    <SessionsPanel on_sessions_loaded={on_sessions_loaded} />
                }
            </main>
        </div>
    }
}

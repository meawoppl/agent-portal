mod appearance_panel;
mod forwarding_panel;
mod health_timer_panel;
mod launchers_panel;
mod notifications_panel;
mod performance_panel;
mod sessions_panel;
mod sounds_panel;
mod tokens_panel;

use crate::utils;
use appearance_panel::AppearancePanel;
use forwarding_panel::ForwardingPanel;
use health_timer_panel::HealthTimerPanel;
use launchers_panel::LaunchersPanel;
use notifications_panel::NotificationsPanel;
use performance_panel::PerformancePanel;
use sessions_panel::SessionsPanel;
use shared::{ProxyTokenInfo, SessionInfo};
use sounds_panel::SoundsPanel;
use tokens_panel::{count_expiring_tokens, TokensPanel};
use yew::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum SettingsTab {
    Sessions,
    Tokens,
    Launchers,
    Forwarding,
    Sounds,
    Notifications,
    HealthTimer,
    Performance,
    Appearance,
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

    // Tab click handlers — one-line closure factory, see `make_sort_handler`
    // in `admin/users_tab.rs` for the pattern.
    let make_tab_handler = |tab: SettingsTab| {
        let active_tab = active_tab.clone();
        Callback::from(move |_: MouseEvent| active_tab.set(tab))
    };
    let on_sessions_tab = make_tab_handler(SettingsTab::Sessions);
    let on_tokens_tab = make_tab_handler(SettingsTab::Tokens);
    let on_launchers_tab = make_tab_handler(SettingsTab::Launchers);
    let on_forwarding_tab = make_tab_handler(SettingsTab::Forwarding);
    let on_sounds_tab = make_tab_handler(SettingsTab::Sounds);
    let on_notifications_tab = make_tab_handler(SettingsTab::Notifications);
    let on_health_timer_tab = make_tab_handler(SettingsTab::HealthTimer);
    let on_performance_tab = make_tab_handler(SettingsTab::Performance);
    let on_appearance_tab = make_tab_handler(SettingsTab::Appearance);

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
                <button class="header-button logout" onclick={Callback::from(|_| utils::logout())}>
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
                    class={classes!("tab-button", (*active_tab == SettingsTab::Launchers).then_some("active"))}
                    onclick={on_launchers_tab}
                >
                    { "Launchers" }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Forwarding).then_some("active"))}
                    onclick={on_forwarding_tab}
                >
                    { "Forwarding" }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Sounds).then_some("active"))}
                    onclick={on_sounds_tab}
                >
                    { "Sounds" }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Notifications).then_some("active"))}
                    onclick={on_notifications_tab}
                >
                    { "Notifications" }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::HealthTimer).then_some("active"))}
                    onclick={on_health_timer_tab}
                >
                    { "Health Timer" }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Performance).then_some("active"))}
                    onclick={on_performance_tab}
                >
                    { "Performance" }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Appearance).then_some("active"))}
                    onclick={on_appearance_tab}
                >
                    { "Appearance" }
                </button>
            </nav>

            <main class="settings-content">
                if *active_tab == SettingsTab::Tokens {
                    <TokensPanel on_tokens_loaded={on_tokens_loaded} />
                }
                if *active_tab == SettingsTab::Launchers {
                    <LaunchersPanel />
                }
                if *active_tab == SettingsTab::Forwarding {
                    <ForwardingPanel />
                }
                if *active_tab == SettingsTab::Sounds {
                    <SoundsPanel />
                }
                if *active_tab == SettingsTab::Notifications {
                    <NotificationsPanel />
                }
                if *active_tab == SettingsTab::HealthTimer {
                    <HealthTimerPanel />
                }
                if *active_tab == SettingsTab::Sessions {
                    <SessionsPanel on_sessions_loaded={on_sessions_loaded} />
                }
                if *active_tab == SettingsTab::Performance {
                    <PerformancePanel />
                }
                if *active_tab == SettingsTab::Appearance {
                    <AppearancePanel />
                }
            </main>
        </div>
    }
}
